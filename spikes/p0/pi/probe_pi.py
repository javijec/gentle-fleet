#!/usr/bin/env python3
"""P0 probe for Gentle Shell (Pi RPC).

Runs against the installed `gentle-shell` (a Pi wrapper). It exercises the parts
Fleet needs in P1 and records what actually happened. Sessions are written to an
isolated `--session-dir` so the user's Gentle Shell history is untouched.

Usage:
    python3 spikes/p0/pi/probe_pi.py --scenario all --capture /tmp/pi-capture.jsonl

Scenarios:
    chat        prompt -> streaming text -> tool execution -> agent_settled
    permission  destructive command -> extension_ui_request select -> Deny
    resume      settle, stop the process, start a new one, switch_session
    disconnect  close stdin mid-turn and record what the engine does

Requires real model credentials for Gentle Shell's configured default provider.
No credential is read, printed, or written by this script.
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

ENGINE = "gentle-shell/pi"
BIN = binary("GENTLE_SHELL_BIN", "gentle-shell")


class Rpc:
    def __init__(self, cwd: Path, session_dir: Path, capture: Capture, label: str):
        env = os.environ.copy()
        env["GENTLE_SHELL_INTERACTIVE_HOST"] = "1"
        self.capture = capture
        self.label = label
        self.next_id = 0
        self.events: list[dict] = []
        self.p = subprocess.Popen(
            [BIN, "--mode", "rpc", "--session-dir", str(session_dir)],
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

    def request(self, command: str, timeout: float = 60, **fields) -> dict:
        self.next_id += 1
        request_id = f"r{self.next_id}"
        self.send({"id": request_id, "type": command, **fields})
        deadline = time.time() + timeout
        while time.time() < deadline:
            event = self.next_event(1)
            if event is None:
                continue
            if event.get("type") == "response" and event.get("id") == request_id:
                self.capture.add(ENGINE, self.label + ".response", event)
                return event
        raise TimeoutError(command)

    def next_event(self, timeout: float = 1.0) -> dict | None:
        try:
            label, line = self.q.get(timeout=timeout)
        except queue.Empty:
            return None
        if label != "out" or not line:
            return None
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            return None
        self.events.append(event)
        return event

    def on_ui_request(self, event: dict, deny: bool = True) -> None:
        method = event.get("method")
        self.capture.add(ENGINE, self.label + ".ui_request", event)
        if method == "select":
            options = event.get("options") or []
            value = "Deny" if deny else options[0] if options else None
            self.send({"type": "extension_ui_response", "id": event["id"], "value": value})
        elif method == "confirm":
            self.send({"type": "extension_ui_response", "id": event["id"], "confirmed": not deny})
        elif method in ("input", "editor"):
            self.send({"type": "extension_ui_response", "id": event["id"], "cancelled": True})
        elif method == "notify":
            print(f"  notify: {str(event.get('message', ''))[:160]}")
        # setStatus / setWidget are presentation only.

    def settle(self, timeout: float = 240, deny: bool = True) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            event = self.next_event(1)
            if event is None:
                continue
            kind = event.get("type")
            if kind == "extension_ui_request":
                self.on_ui_request(event, deny=deny)
            elif kind == "message_update":
                delta = (event.get("assistantMessageEvent") or {}).get("delta")
                if delta:
                    self.capture.add(ENGINE, self.label + ".text_delta", {"delta": delta})
            elif kind in ("tool_execution_start", "tool_execution_end", "tool_execution_update"):
                self.capture.add(ENGINE, self.label + ".tool", event)
            elif kind in ("agent_start", "turn_start", "turn_end", "agent_settled"):
                self.capture.add(ENGINE, self.label + ".lifecycle", {"type": kind})
            if kind == "agent_settled":
                return
        raise TimeoutError("agent_settled")

    def stop(self) -> int:
        try:
            if self.p.stdin:
                self.p.stdin.close()
        except OSError:
            pass
        try:
            return self.p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.p.kill()
            return -1


def set_normal_mode(rpc: Rpc) -> None:
    response = rpc.request("prompt", message="/gentle:mode normal")
    require(
        response.get("data", {}).get("disposition") in ("handled", "started"),
        f"unexpected /gentle:mode response: {response}",
    )


def scenario_chat(root: Path, capture: Capture) -> None:
    cwd, sessions = root / "cwd", root / "sessions"
    rpc = Rpc(cwd, sessions, capture, "chat")
    try:
        state = rpc.request("get_state")
        require(bool(state.get("data")), "get_state returned no data")
        set_normal_mode(rpc)
        response = rpc.request(
            "prompt",
            message="Use the bash tool to run exactly: echo gf-pi > p0-pi.txt. Then stop.",
        )
        require(response.get("data", {}).get("disposition") == "started", "prompt not started")
        rpc.settle()
        require((cwd / "p0-pi.txt").exists(), "chat tool did not create p0-pi.txt")
        print("  chat: ok")
    finally:
        rpc.stop()


def scenario_permission(root: Path, capture: Capture) -> None:
    cwd, sessions = root / "cwd", root / "sessions"
    victim = cwd / "victimdir"
    victim.mkdir()
    (victim / "a.txt").write_text("x")
    rpc = Rpc(cwd, sessions, capture, "permission")
    try:
        set_normal_mode(rpc)
        rpc.request("prompt", message="Use the bash tool to run exactly: rm -r victimdir. Then stop.")
        rpc.settle(deny=True)
        require(victim.exists(), "denied destructive command still removed victimdir")
        print("  permission: ok (denied, directory intact)")
    finally:
        rpc.stop()


def scenario_resume(root: Path, capture: Capture) -> None:
    cwd, sessions = root / "cwd", root / "sessions"
    rpc = Rpc(cwd, sessions, capture, "resume")
    session_file = None
    try:
        state = rpc.request("get_state")
        session_file = (state.get("data") or {}).get("sessionFile")
        require(bool(session_file), "no sessionFile even though --session-dir was used")
        set_normal_mode(rpc)
        rpc.request("prompt", message="Reply with exactly: persisted-p0. Then stop.")
        rpc.settle()
        print(f"  session file: {session_file}")
    finally:
        rpc.stop()
    require(Path(session_file).exists(), "session file missing after settle")

    rpc = Rpc(cwd, sessions, capture, "resume-reopen")
    try:
        response = rpc.request("switch_session", sessionPath=session_file)
        require(response.get("data", {}).get("cancelled") is False, "switch_session cancelled")
        messages = rpc.request("get_messages").get("data", {}).get("messages", [])
        texts = [
            part.get("text")
            for message in messages
            if message.get("role") == "assistant"
            for part in (message.get("content") or [])
            if isinstance(part, dict)
        ]
        require(any(text and "persisted-p0" in text for text in texts), "resumed history incomplete")
        print("  resume: ok")
    finally:
        rpc.stop()


def scenario_disconnect(root: Path, capture: Capture) -> None:
    cwd, sessions = root / "cwd", root / "sessions"
    rpc = Rpc(cwd, sessions, capture, "disconnect")
    set_normal_mode(rpc)
    rpc.send({"type": "prompt", "message": "Run exactly: sleep 20 && echo late > late-p0.txt. Then stop."})
    time.sleep(3)
    require(rpc.p.poll() is None, "process already dead before disconnect")
    assert rpc.p.stdin is not None
    rpc.p.stdin.close()
    try:
        code = rpc.p.wait(timeout=15)
    except subprocess.TimeoutExpired:
        rpc.p.kill()
        require(False, "process survived stdin close (unexpected for stdio RPC)")
    time.sleep(1)
    capture.add(
        ENGINE,
        "disconnect.result",
        {"exit_code": code, "late_file_created": (cwd / "late-p0.txt").exists()},
    )
    require(not (cwd / "late-p0.txt").exists(), "work continued after stdin close")
    print("  disconnect: ok (process exits, in-flight work stops)")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--scenario",
        choices=["all", "chat", "permission", "resume", "disconnect"],
        default="all",
    )
    parser.add_argument("--capture", help="Path for the anonymised JSONL capture")
    args = parser.parse_args()

    capture = Capture(args.capture)
    root = workdir("gf-p0-pi-")
    print(f"{ENGINE} probe: {BIN}")
    print(f"workspace: {root}")
    try:
        scenarios = ["chat", "permission", "resume", "disconnect"]
        if args.scenario != "all":
            scenarios = [args.scenario]
        for name in scenarios:
            print(f"[{name}]")
            globals()[f"scenario_{name}"](root, capture)
    finally:
        if not os.environ.get("GF_KEEP_WORKDIR"):
            shutil.rmtree(root, ignore_errors=True)
    print("PASS")


if __name__ == "__main__":
    main()
