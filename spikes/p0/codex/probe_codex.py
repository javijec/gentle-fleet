#!/usr/bin/env python3
"""P0 probe for the Codex app-server (JSON-RPC over stdio).

The app-server protocol is experimental and version specific. This probe pins the
observed handshake, turn lifecycle, command approval and cross-process resume for
the installed `codex-cli`.

Usage:
    python3 spikes/p0/codex/probe_codex.py --capture /tmp/codex-capture.jsonl

Requires a logged-in Codex account (`codex login status`). The script does not
read or print credentials. Threads are real Codex threads, so they are written to
`$CODEX_HOME/sessions`; the probe sets `CODEX_HOME` to a temporary directory to
avoid touching the user's own history and links the existing `auth.json` into it
so the real account is used without copying or printing the secret.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import Capture, binary, require, workdir  # noqa: E402

ENGINE = "codex"
BIN = binary("CODEX_BIN", "codex")


class AppServer:
    def __init__(self, cwd: Path, codex_home: Path, capture: Capture):
        env = os.environ.copy()
        env["CODEX_HOME"] = str(codex_home)
        self.capture = capture
        self.next_id = 0
        self.deprecations: list[dict] = []
        self.p = subprocess.Popen(
            [BIN, "app-server"],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.q: queue.Queue[tuple[str, bytes]] = queue.Queue()
        threading.Thread(target=self._read, args=(self.p.stdout, "out"), daemon=True).start()
        threading.Thread(target=self._read, args=(self.p.stderr, "err"), daemon=True).start()

    def _read(self, stream, label):
        for line in iter(stream.readline, b""):
            self.q.put((label, line.rstrip(b"\r\n")))

    def send(self, message: dict) -> None:
        assert self.p.stdin is not None
        self.p.stdin.write((json.dumps(message) + "\n").encode())
        self.p.stdin.flush()

    def request(self, method: str, params: dict | None = None, timeout: float = 60) -> dict:
        self.next_id += 1
        request_id = str(self.next_id)
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}})
        deadline = time.time() + timeout
        while time.time() < deadline:
            message = self.next_message(1)
            if message is None:
                continue
            if message.get("id") == request_id and ("result" in message or "error" in message):
                self.capture.add(ENGINE, "response." + method, message)
                return message
            self.handle(message)
        raise TimeoutError(method)

    def next_message(self, timeout: float = 1.0) -> dict | None:
        try:
            label, line = self.q.get(timeout=timeout)
        except queue.Empty:
            return None
        if label != "out" or not line:
            return None
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            return None

    def handle(self, message: dict, accept: bool = True) -> None:
        method = message.get("method")
        if not method:
            return
        self.capture.add(ENGINE, "notification." + method, message)
        if method == "deprecationNotice":
            self.deprecations.append(message.get("params") or {})
        if "id" not in message:
            return
        request_id = message["id"]
        if method.endswith("requestApproval"):
            decision = "accept" if accept else "decline"
            self.send({"jsonrpc": "2.0", "id": request_id, "result": {"decision": decision}})
            self.capture.add(ENGINE, "approval.reply", {"method": method, "decision": decision})
        elif method.endswith("requestUserInput"):
            self.send({"jsonrpc": "2.0", "id": request_id, "result": {"answers": {}}})
        else:
            self.send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "not handled by P0 probe"},
                }
            )

    def wait_turn(self, timeout: float = 240, accept: bool = True) -> dict:
        deadline = time.time() + timeout
        while time.time() < deadline:
            message = self.next_message(1)
            if message is None:
                continue
            self.handle(message, accept=accept)
            if message.get("method") == "turn/completed":
                return message
        raise TimeoutError("turn/completed")

    def stop(self) -> None:
        try:
            if self.p.stdin:
                self.p.stdin.close()
        except OSError:
            pass
        try:
            self.p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.p.kill()


def initialize(app: AppServer) -> None:
    result = app.request(
        "initialize",
        {
            "clientInfo": {"name": "gentle-fleet-p0", "version": "0.1.0"},
            "capabilities": {"experimentalApi": False},
        },
    )
    require("result" in result, f"initialize failed: {result}")
    app.send({"jsonrpc": "2.0", "method": "initialized"})


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture", help="Path for the anonymised JSONL capture")
    args = parser.parse_args()

    capture = Capture(args.capture)
    root = workdir("gf-p0-codex-")
    cwd = root / "cwd"
    codex_home = Path(os.environ["GF_CODEX_HOME"]) if os.environ.get("GF_CODEX_HOME") else root / "codex-home"
    codex_home.mkdir(parents=True, exist_ok=True)
    if not os.environ.get("GF_CODEX_HOME"):
        # Keep the real login, but isolate the threads the probe creates.
        real_home = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
        auth = real_home / "auth.json"
        if auth.exists() and not (codex_home / "auth.json").exists():
            (codex_home / "auth.json").symlink_to(auth)
    print(f"{ENGINE} probe: {BIN}")
    print(f"workspace: {root}")
    try:
        app = AppServer(cwd, codex_home, capture)
        try:
            initialize(app)
            started = app.request(
                "thread/start",
                {
                    "cwd": str(cwd),
                    "approvalPolicy": "on-request",
                    "sandbox": "read-only",
                },
            )
            require("result" in started, f"thread/start failed: {started}")
            thread_id = started["result"]["thread"]["id"]
            print(f"  thread: {thread_id}")

            turn = app.request(
                "turn/start",
                {
                    "threadId": thread_id,
                    "input": [
                        {
                            "type": "text",
                            "text": "Run exactly this shell command: echo gf-codex > p0-codex.txt. Then stop.",
                        }
                    ],
                },
            )
            require("result" in turn, f"turn/start failed: {turn}")
            completed = app.wait_turn(accept=True)
            require(
                completed["params"]["turn"]["status"] == "completed",
                f"turn ended as {completed['params']['turn']['status']}: {completed['params']['turn'].get('error')}",
            )
            require((cwd / "p0-codex.txt").exists(), "approved command did not create p0-codex.txt")
            print("  turn + approval: ok")

            read = app.request("thread/read", {"threadId": thread_id, "includeTurns": True})
            require("result" in read, "thread/read failed")
            print(f"  thread/read: ok (deprecation notices: {bool(app.deprecations)})")
        finally:
            app.stop()

        # Cross-process resume: a fresh app-server must find the persisted thread.
        app = AppServer(cwd, codex_home, capture)
        try:
            initialize(app)
            resumed = app.request("thread/resume", {"threadId": thread_id})
            require("result" in resumed, f"thread/resume failed: {resumed}")
            turns = app.request("thread/turns/list", {"threadId": thread_id})
            require("result" in turns, "thread/turns/list failed")
            require(len(turns["result"]["data"]) >= 1, "resumed thread has no turns")
            print("  resume: ok")
        finally:
            app.stop()
        print("PASS")
    finally:
        if not os.environ.get("GF_KEEP_WORKDIR"):
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
