#!/usr/bin/env python3
"""P0 probe for the OpenCode v2 HTTP API.

Starts an isolated `opencode serve` on loopback, subscribes to `/api/event`
(SSE), creates a session in a temporary workspace, streams a turn, resolves a
real permission request, restarts the server and verifies the session is still
readable. Optionally checks the user-level OpenCode service instead.

Usage:
    python3 spikes/p0/opencode/probe_opencode.py --capture /tmp/opencode-capture.jsonl
    python3 spikes/p0/opencode/probe_opencode.py --attach

Notes:
    * OpenCode persists sessions in the user's OpenCode store, so the probe
      deletes the sessions it creates before exiting.
    * The server password is read from the process output and never printed or
      written to a capture.
"""

from __future__ import annotations

import argparse
import base64
import http.client
import json
import os
import queue
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import Capture, binary, require, workdir  # noqa: E402

ENGINE = "opencode"
BIN = binary("OPENCODE_BIN", "opencode")


def free_port() -> int:
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def auth_header(password: str) -> str:
    token = base64.b64encode(f"opencode:{password}".encode()).decode()
    return "Basic " + token


class Server:
    def __init__(self, root: Path, port: int | None = None):
        self.port = port or free_port()
        self.proc = subprocess.Popen(
            [BIN, "serve", "--hostname", "127.0.0.1", "--port", str(self.port)],
            cwd=root,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.password = None
        lines: queue.Queue[str] = queue.Queue()

        def read_stdout():
            for line in self.proc.stdout:
                lines.put(line)

        threading.Thread(target=read_stdout, daemon=True).start()
        deadline = time.time() + 30
        listening = False
        while time.time() < deadline and not (self.password and listening):
            try:
                line = lines.get(timeout=0.5)
            except queue.Empty:
                if self.proc.poll() is not None:
                    break
                continue
            if line.startswith("server password "):
                self.password = line.split("server password ", 1)[1].strip()
            if line.startswith("server listening on"):
                listening = True
        if not self.password:
            self.stop()
            raise RuntimeError("opencode serve did not print a password")
        # stderr is only read on failure.
        threading.Thread(target=self._drain, args=(self.proc.stderr,), daemon=True).start()

    def _drain(self, stream):
        for _ in stream:
            pass

    def request(self, method: str, path: str, body=None, timeout: float = 60):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        headers = {"Authorization": auth_header(self.password)}
        data = None
        if body is not None:
            data = json.dumps(body)
            headers["Content-Type"] = "application/json"
        conn.request(method, path, body=data, headers=headers)
        response = conn.getresponse()
        raw = response.read()
        conn.close()
        try:
            payload = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            payload = raw[:400].decode("utf-8", "replace")
        return response.status, payload

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class EventStream:
    def __init__(self, server: Server, capture: Capture):
        self.server = server
        self.capture = capture
        self.events: list[dict] = []
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()
        time.sleep(0.5)

    def _run(self):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=None)
        conn.request(
            "GET",
            "/api/event",
            headers={
                "Authorization": auth_header(self.server.password),
                "Accept": "text/event-stream",
            },
        )
        response = conn.getresponse()
        while not self.stop.is_set():
            line = response.readline()
            if not line:
                break
            if not line.startswith(b"data: "):
                continue
            try:
                event = json.loads(line[6:])
            except json.JSONDecodeError:
                continue
            with self.lock:
                self.events.append(event)
            if event.get("type") not in ("session.text.delta", "session.reasoning.delta", "session.tool.input.delta"):
                self.capture.add(ENGINE, "event." + str(event.get("type")), event)
        conn.close()

    def snapshot(self) -> list[dict]:
        with self.lock:
            return list(self.events)

    def close(self):
        self.stop.set()


def wait_for(stream: EventStream, session_id: str, timeout: float = 300) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        for event in stream.snapshot():
            data = event.get("data") or {}
            if data.get("sessionID") != session_id:
                continue
            if event.get("type") in (
                "session.execution.succeeded",
                "session.execution.failed",
                "session.execution.interrupted",
            ):
                return
        time.sleep(0.5)
    raise TimeoutError("session execution did not finish")


def answer_permissions(server: Server, session_id: str, decision: str, seen: set[str]) -> int:
    status, payload = server.request("GET", f"/api/session/{session_id}/permission")
    if status != 200:
        return 0
    replied = 0
    for request in payload.get("data", []):
        if request["id"] in seen:
            continue
        seen.add(request["id"])
        code, _ = server.request(
            "POST",
            f"/api/session/{session_id}/permission/{request['id']}/reply",
            {"decision": decision},
        )
        require(code == 204, f"permission reply failed with {code}")
        replied += 1
    return replied


def create_session(server: Server, cwd: Path, title: str, permissions=None) -> str:
    body = {"title": title, "location": {"directory": str(cwd)}}
    if permissions:
        body["permissions"] = permissions
    status, payload = server.request("POST", "/api/session", body)
    require(status == 200, f"session create failed: {status} {payload}")
    return payload["data"]["id"]


def run_turn(server: Server, stream: EventStream, session_id: str, text: str, decision: str | None) -> None:
    status, payload = server.request("POST", f"/api/session/{session_id}/prompt", {"text": text})
    require(status == 200, f"prompt failed: {status} {payload}")
    seen: set[str] = set()
    deadline = time.time() + 300
    while time.time() < deadline:
        if decision:
            answer_permissions(server, session_id, decision, seen)
        for event in stream.snapshot():
            data = event.get("data") or {}
            if data.get("sessionID") == session_id and event.get("type") in (
                "session.execution.succeeded",
                "session.execution.failed",
                "session.execution.interrupted",
            ):
                return
        time.sleep(0.5)
    raise TimeoutError("turn did not finish")


def delete_session(server: Server, session_id: str) -> None:
    server.request("DELETE", f"/api/session/{session_id}")


def probe_auto(root: Path, capture: Capture) -> None:
    cwd = root / "cwd"
    server = Server(root)
    stream = EventStream(server, capture)
    session_ids: list[str] = []
    try:
        status, info = server.request("GET", "/api/info")
        require(status == 200 and info.get("version"), "GET /api/info failed")
        print(f"  server: {info['version']} pid={info['pid']}")

        chat_id = create_session(server, cwd, "gf-p0-chat")
        session_ids.append(chat_id)
        run_turn(
            server,
            stream,
            chat_id,
            "Run exactly this shell command: echo gf-opencode > p0-oc.txt. Then stop.",
            decision=None,
        )
        require((cwd / "p0-oc.txt").exists(), "OpenCode turn did not create p0-oc.txt")
        print("  chat: ok")

        accept_id = create_session(
            server, cwd, "gf-p0-permission-accept", [{"action": "shell", "resource": "*", "effect": "ask"}]
        )
        session_ids.append(accept_id)
        run_turn(
            server,
            stream,
            accept_id,
            "Run exactly this shell command: echo gf-accepted > p0-accept.txt. Then stop.",
            decision="once",
        )
        require((cwd / "p0-accept.txt").exists(), "accepted permission did not run the command")
        print("  permission accept: ok")

        reject_id = create_session(
            server, cwd, "gf-p0-permission-reject", [{"action": "shell", "resource": "*", "effect": "ask"}]
        )
        session_ids.append(reject_id)
        run_turn(
            server,
            stream,
            reject_id,
            "Run exactly this shell command: echo gf-denied > p0-deny.txt. Then stop.",
            decision="reject",
        )
        require(not (cwd / "p0-deny.txt").exists(), "rejected permission still ran the command")
        print("  permission reject: ok")
        stream.close()

        port = server.port
        server.stop()
        time.sleep(1)
        server = Server(root, port=port)
        status, payload = server.request("GET", f"/api/session/{chat_id}")
        require(status == 200 and payload["data"]["id"] == chat_id, "session not readable after restart")
        status, messages = server.request("GET", f"/api/session/{chat_id}/message?limit=5")
        require(status == 200 and len(messages.get("data", [])) >= 2, "messages missing after restart")
        print("  restart persistence: ok")
    finally:
        stream.close()
        for session_id in session_ids:
            try:
                delete_session(server, session_id)
            except Exception:
                pass
        server.stop()


def probe_attach(capture: Capture) -> None:
    state = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "opencode" / "service.json"
    require(state.exists(), f"no user-level OpenCode service at {state}")
    service = json.loads(state.read_text())
    conn = http.client.HTTPConnection(service["url"].split("//", 1)[1], timeout=10)
    conn.request(
        "GET",
        "/api/info",
        headers={"Authorization": auth_header(service["password"])},
    )
    response = conn.getresponse()
    payload = json.loads(response.read())
    conn.close()
    require(response.status == 200, "attaching to the user service failed")
    capture.add(ENGINE, "attach.info", payload)
    print(f"  attach: ok ({payload['version']} pid={payload['pid']})")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture", help="Path for the anonymised JSONL capture")
    parser.add_argument("--attach", action="store_true", help="Only verify the user-level service")
    args = parser.parse_args()

    capture = Capture(args.capture)
    print(f"{ENGINE} probe: {BIN}")
    if args.attach:
        probe_attach(capture)
        print("PASS")
        return

    root = workdir("gf-p0-opencode-")
    print(f"workspace: {root}")
    try:
        probe_auto(root, capture)
        print("PASS")
    finally:
        if not os.environ.get("GF_KEEP_WORKDIR"):
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
