#!/usr/bin/env python3
"""Opt-in native Windows Codex chat smoke. May incur model charges.

No credentials are read, copied or printed by this harness. A failed stop
keeps the isolated daemon and data available for explicit reconciliation.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real", action="store_true", help="Authorize one real model turn")
    parser.add_argument("--codex-bin", type=Path, help="Absolute native codex.exe (not a shell shim)")
    args = parser.parse_args()
    if not args.real or os.name != "nt":
        print("BLOCKED: requires native Windows and explicit --real")
        return 1
    if args.codex_bin and (not args.codex_bin.is_absolute()
                           or args.codex_bin.suffix.lower() != ".exe"
                           or not args.codex_bin.is_file()):
        print("BLOCKED: --codex-bin must identify an existing absolute .exe")
        return 1
    bin_dir = ROOT / "target" / "debug"
    if any(not (bin_dir / exe).is_file() for exe in ("fleetd.exe", "fleetctl.exe", "fleet-runner.exe")):
        print("BLOCKED: build fleetd, fleetctl and fleet-runner first")
        return 1
    root = Path(tempfile.mkdtemp(prefix="fleet w2 codex "))
    binaries = root / "isolated binaries"
    workspace = root / "workspace with spaces"
    binaries.mkdir()
    workspace.mkdir()
    for exe in ("fleetd.exe", "fleetctl.exe", "fleet-runner.exe"):
        shutil.copy2(bin_dir / exe, binaries / exe)
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("FLEET_"):
            del env[key]
    env.update(
        FLEET_RUNTIME_DIR=str(root / "runtime"),
        FLEET_DB_PATH=str(root / "private data" / "fleet.sqlite"),
        FLEET_OPERATION_JOURNAL=str(root / "private client" / "operations.jsonl"),
        FLEET_CURSOR_JOURNAL=str(root / "private client" / "cursors.jsonl"),
        # Never relax the native sandbox or approval rules to obtain a PASS.
        FLEET_CODEX_SANDBOX="read-only",
    )
    if args.codex_bin:
        env["FLEET_CODEX_BIN"] = str(args.codex_bin)
    daemon = subprocess.Popen([str(binaries / "fleetd.exe")], env=env, cwd=workspace,
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL)
    session = None
    create_attempted = False
    stopped = False
    passed = False

    def cli(*argv, timeout=90):
        result = subprocess.run([str(binaries / "fleetctl.exe"), *argv], cwd=workspace,
                                env=env, capture_output=True, text=True, encoding="utf-8",
                                errors="replace", timeout=timeout)
        if result.returncode:
            # Provider stderr may include native IDs and secrets. Do not print it.
            raise RuntimeError(f"CLI {argv[0]} failed (exit {result.returncode})")
        return result.stdout.rstrip("\r\n")

    def require(condition, message):
        if not condition:
            raise RuntimeError(message)

    try:
        deadline = time.monotonic() + 30
        while not (root / "runtime" / "fleetd.addr").is_file():
            require(daemon.poll() is None and time.monotonic() < deadline,
                    "daemon readiness failed")
            time.sleep(.1)
        # ListAgents is the real public discovery API; it also probes the
        # other installed providers. Only Codex is evaluated in this cut.
        agents = [line.split("\t") for line in cli("agents", timeout=120).splitlines()]
        codex = [row for row in agents if row and row[0] == "codex"]
        require(len(codex) == 1 and len(codex[0]) == 5
                and codex[0][2] == "Available" and codex[0][1] not in ("unknown", "0.0.0"),
                "Codex discovery/version unavailable or incompatible")
        print("PASS: native Codex discovery; version", codex[0][1])
        ws = cli("workspace", "add", str(workspace)).split("\t", 1)[0]
        create_attempted = True
        receipt = cli("session", "create", "--workspace", ws, "--provider", "codex").split("\t")
        require(len(receipt) == 4 and receipt[1:3] == ["codex", "Running"],
                "session creation not confirmed")
        session = receipt[0]
        key = uuid.uuid4().hex
        env["FLEET_OPERATION_KEY"] = key
        marker = "FLEET W2 CODEX OK"
        message = "Reply with exactly: " + marker + ". Do not use any tools."
        prompt = cli("prompt", session, message).split("\t")
        require(len(prompt) == 3 and prompt[1] == session, "prompt receipt mismatch")
        deadline = time.monotonic() + 180
        while True:
            timeline = cli("timeline", session, "--since", "0", "--limit", "256")
            rows = [line.split("\t", 2) for line in timeline.splitlines()]
            require(all(len(row) == 3 for row in rows), "invalid timeline row")
            payloads = [row[2] for row in rows]
            terminals = [p for p in payloads if p.startswith("turn_completed\t")]
            if terminals:
                break
            require(not any(p.startswith("approval_requested\t") for p in payloads),
                    "unexpected approval: not automatically answered")
            require(time.monotonic() < deadline, "native model turn did not complete")
            time.sleep(.2)
        require(terminals == ["turn_completed\tCompleted"], "turn did not complete successfully")
        require(payloads.count("user\t" + message) == 1, "prompt missing or duplicated")
        require(any(p.startswith("delta\t") for p in payloads), "no streamed assistant delta")
        deltas = "".join(p.split("\t", 1)[1] for p in payloads if p.startswith("delta\t"))
        assistants = [p.split("\t", 1)[1] for p in payloads if p.startswith("assistant\t")]
        require(marker in deltas or any(marker in p for p in assistants), "expected answer missing")
        require(not any(p.startswith(("tool_started\t", "approval_requested\t")) for p in payloads),
                "chat-only smoke executed or requested a tool")
        for i, row in enumerate(rows, 1):
            require(row[:2] == [str(i), f"{session}-e{i}"], "invalid event sequence or identity")
        op = cli("operation", key).split("\t")
        require(len(op) >= 4 and op[0] == prompt[0] and op[1] == key and op[3] == "Completed",
                "operation not durably completed")
        replay = cli("watch", session, "--since", "0", "--count", str(len(rows)))
        require(replay.splitlines() == [row[0] + "\t" + row[2] for row in rows],
                "durable replay differs")
        print("PASS: real Codex prompt, streaming, terminal, operation and exact replay")
        passed = True
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"FAIL: {type(error).__name__}: {error}")
    finally:
        if session:
            try:
                stopped = cli("stop", session) == "stopped\t" + session
                if stopped:
                    rows = [row.split("\t") for row in cli("session", "list").splitlines()]
                    stopped = any(len(row) >= 4 and row[0] == session and row[3] == "Stopped" for row in rows)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                stopped = False
        if stopped or not create_attempted:
            if daemon.poll() is None:
                daemon.terminate()
                daemon.wait(timeout=10)
            shutil.rmtree(root)
        else:
            print("BLOCKED: stop/startup outcome unconfirmed; fixture retained for reconciliation")
            print("Fixture:", root)
            print("Daemon PID (diagnostic only; do not kill by PID):", daemon.pid)
            print("Use the fixture runtime and fleetctl to inspect/stop its session explicitly.")
    if passed and stopped:
        print("PASS: W2-A native Codex chat and confirmed stop")
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
