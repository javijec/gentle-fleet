#!/usr/bin/env python3
"""Opt-in native Windows smoke for all real Fleet providers.

One isolated Fleet daemon/workspace per provider. No install, automatic
approval, prompt retry, global permission changes, or raw provider logs.
Use --real to authorize model calls. Extra checks are explicitly opt-in.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
PROVIDERS = ("opencode", "codex", "pi", "gentle-shell")
EXES = ("fleetd.exe", "fleet-runner.exe", "fleetctl.exe")
RANGES = {"opencode": (2, 3, 3), "codex": (0, 171, 1),
          "pi": (1, 6, 2), "gentle-shell": (4, 8, 5)}
LAUNCH_KEYS = ("FLEET_OPENCODE_BIN", "FLEET_CODEX_BIN", "FLEET_PI_BIN",
               "FLEET_PI_JS", "FLEET_GENTLE_SHELL_BIN",
               "FLEET_GENTLE_SHELL_JS", "FLEET_NODE_BIN")


class Failure(RuntimeError):
    pass


class Blocked(Failure):
    pass


def require(condition, message):
    if not condition:
        raise Failure(message)


def compatible(provider, version, experimental_gentle_shell_5=False):
    if provider == "gentle-shell" and experimental_gentle_shell_5 and version == "5.0.0":
        return True
    match = re.match(r"^(\d+)\.(\d+)(?:[.-]|$)", version or "")
    if not match:
        return False
    major, minor = map(int, match.groups())
    lo_major, lo_minor, hi_major = RANGES[provider]
    return (major, minor) >= (lo_major, lo_minor) and major < hi_major


def version_label(value):
    # Never echo arbitrary command output.
    return value if isinstance(value, str) and re.fullmatch(
        r"\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]{1,96})?", value) else "unknown"


def jsonl(text):
    return [json.loads(line) for line in text.split("\n") if line]


def stop_reason(text):
    # Fixed labels only. Never include native messages, URLs, IDs or tokens.
    reasons = (
        ("provider exited with active descendants", "active_descendants"),
        ("provider job did not become empty", "job_not_empty"),
        ("required forced termination", "forced_termination"),
        ("exited unsuccessfully after close", "provider_nonzero_exit"),
        ("driver did not close in time", "driver_close_timeout"),
        ("driver worker exited without close confirmation", "driver_worker_disconnected"),
        ("driver command channel disconnected", "driver_command_disconnected"),
        ("driver command queue full", "driver_queue_full"),
        ("waiting for runner RPC lock", "runner_rpc_lock_timeout"),
        ("runner_rpc_timeout", "runner_rpc_timeout"),
        ("runner did not acknowledge", "runner_shutdown_ack_missing"),
        ("runner exit cannot be verified", "runner_exit_unverified"),
        ("runner closed the connection", "runner_connection_closed"),
        ("StopUnconfirmed", "stop_unconfirmed"),
    )
    return next((label for text_part, label in reasons if text_part in text), "unknown")


def error_classes(kinds):
    """Return fixed diagnostic categories only, never native text or paths."""
    patterns = (
        ("pi_handled_without_model", r"Pi prompt disposition=handled"),
        ("sandbox", r"\bsandbox\b|sandboxing"),
        ("access_denied", r"access (?:is )?denied|permission denied|unauthorized|eacces|eperm"),
        ("auth", r"\b401\b|authentication|not authenticated|api key|oauth|credential|not logged in"),
        ("rate_limit", r"\b429\b|rate.limit|quota|insufficient credits"),
        ("model", r"model not found|unknown model|no model|unsupported model|model unavailable"),
        ("missing_file", r"cannot find path|no such file|file not found|enoent"),
        ("command_missing", r"not recognized|command not found|not installed"),
        ("network", r"connection refused|network error|fetch failed|econnreset|timed out"),
        ("provider_error", r"\berror\b|failed|failure"),
    )
    labels = set()
    for kind in kinds:
        if kind.get("type") not in ("agent_error", "agent_notice", "turn_failed", "tool_output"):
            continue
        value = kind.get("message", kind.get("output", ""))
        if not isinstance(value, str):
            continue
        for label, pattern in patterns:
            if re.search(pattern, value, re.IGNORECASE):
                labels.add(label)
    return sorted(labels)


def turn_diagnostics(kinds, marker, context=()):
    deltas = [k["text"] for k in kinds if k["type"] == "assistant_delta"]
    messages = [k["text"] for k in kinds if k["type"] == "assistant_message"]
    return {
        "delta_count": len(deltas),
        "delta_chars": sum(map(len, deltas)),
        "message_count": len(messages),
        "message_chars": sum(map(len, messages)),
        "marker_in_deltas": marker in "".join(deltas) if marker else None,
        "marker_in_messages": any(marker in text for text in messages) if marker else None,
        "tool_started": sum(k["type"] == "tool_started" for k in kinds),
        "tool_succeeded": sum(k["type"] == "tool_completed" and k.get("success") is True for k in kinds),
        "tool_failed": sum(k["type"] == "tool_completed" and k.get("success") is False for k in kinds),
        "tool_output_count": sum(k["type"] == "tool_output" for k in kinds),
        "agent_errors": sum(k["type"] == "agent_error" for k in (*kinds, *context)),
        "error_classes": error_classes((*kinds, *context)),
        "terminal_statuses": [k["status"] for k in kinds if k["type"] == "turn_completed"],
    }


def discover_paths(args):
    env = {k: os.environ[k] for k in LAUNCH_KEYS if k in os.environ}
    overrides = {
        "FLEET_OPENCODE_BIN": args.opencode_bin,
        "FLEET_CODEX_BIN": args.codex_bin,
        "FLEET_PI_BIN": args.pi_bin,
        "FLEET_PI_JS": args.pi_js,
        "FLEET_GENTLE_SHELL_BIN": args.gentle_shell_bin,
        "FLEET_GENTLE_SHELL_JS": args.gentle_shell_js,
        "FLEET_NODE_BIN": args.node_bin,
    }
    for key, path in overrides.items():
        if path is not None:
            env[key] = str(path)
    # Known layouts from the native discovery audit. Paths are candidates,
    # never shell snippets. Explicit settings always take precedence.
    home = Path.home()
    candidates = {
        "FLEET_OPENCODE_BIN": home / ".bun/bin/opencode.exe",
        "FLEET_CODEX_BIN": Path(os.environ.get("LOCALAPPDATA", str(home))) /
            "Programs/OpenAI/Codex/bin/codex.exe",
        "FLEET_PI_JS": home / ".pi/agent/bin/pi-launcher.js",
        "FLEET_NODE_BIN": Path(os.environ.get("ProgramFiles", r"C:\Program Files")) /
            "nodejs/node.exe",
    }
    for key, path in candidates.items():
        if key in env or (key.endswith("_JS") and key[:-3] + "_BIN" in env):
            continue
        if path.is_absolute() and path.is_file():
            env[key] = str(path)
    return env


class Fixture:
    def __init__(self, provider, launch, args, row):
        self.provider = provider
        self.args = args
        self.row = row
        self.root = Path(tempfile.mkdtemp(prefix=f"fleet w2 {provider} "))
        self.bin = self.root / "isolated binaries"
        self.workspace = self.root / "workspace with spaces"
        self.bin.mkdir()
        self.workspace.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.upper().startswith("FLEET_")}
        self.env.update(launch)
        self.env.update(
            FLEET_RUNTIME_DIR=str(self.root / "runtime"),
            FLEET_DB_PATH=str(self.root / "private data" / "fleet.sqlite"),
            FLEET_OPERATION_JOURNAL=str(self.root / "private client" / "operations.jsonl"),
            FLEET_CURSOR_JOURNAL=str(self.root / "private client" / "cursors.jsonl"),
            FLEET_CODEX_SANDBOX="read-only",
            FLEET_OPENCODE_MODE=args.opencode_mode,
            FLEET_W2_DIAGNOSTICS="1",
        )
        if args.experimental_gentle_shell_5:
            self.env["FLEET_GENTLE_SHELL_EXPERIMENTAL_5"] = "1"
        self.daemon = None
        self.discovery_reasons = []
        self.session = None
        self.create_attempted = False
        self.stopped = False

    def cli(self, *argv, timeout=90):
        try:
            result = subprocess.run(
                [str(self.bin / "fleetctl.exe"), *argv],
                cwd=self.workspace, env=self.env, capture_output=True,
                text=True, encoding="utf-8", errors="strict", timeout=timeout)
        except subprocess.TimeoutExpired:
            raise Failure(f"CLI {argv[0]} timed out; no automatic retry of mutations") from None
        if result.returncode:
            # Print only a known Fleet error code, not arbitrary provider text.
            codes = ("StopUnconfirmed", "provider_unavailable", "incompatible_version",
                     "provider_protocol", "runner_startup_timeout", "runner_io",
                     "TurnAlreadyRunning", "Unsupported", "unsupported_capability",
                     "OutcomeUnknown", "Unauthorized", "runner_rpc_timeout",
                     "invalid_workspace", "InvalidArgument")
            code = next((c for c in codes if re.search(
                r"(?<![\w])" + re.escape(c) + r"(?![\w])", result.stderr)), "unknown")
            detail = f"; reason={stop_reason(result.stderr)}" if argv[0] == "stop" else ""
            if argv[0] == "stop":
                exit_code = re.search(r"\bexit_code=(-?\d{1,10}|unknown)\b", result.stderr)
                stderr_class = re.search(
                    r"\bstderr_class=(EPIPE|ERR_STREAM_DESTROYED|ERR_STREAM_WRITE_AFTER_END|ERR_USE_AFTER_CLOSE|EBADF|unknown)\b",
                    result.stderr)
                if exit_code:
                    detail += "; provider_exit=" + exit_code.group(1)
                if stderr_class:
                    detail += "; stderr_class=" + stderr_class.group(1)
            raise Failure(f"CLI {argv[0]} exit {result.returncode}; code={code}{detail}")
        return result.stdout.rstrip("\r\n")

    def mark(self, stage, status="PASS", detail=None):
        entry = {"stage": stage, "status": status}
        if detail:
            entry["detail"] = detail
        self.row["checks"].append(entry)
        print(f"{status}: {self.provider} {stage}" + (f" — {detail}" if detail else ""),
              flush=True)

    def start_daemon(self):
        self.daemon = subprocess.Popen(
            [str(self.bin / "fleetd.exe")], cwd=self.workspace, env=self.env,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        # Drain daemon stderr, retaining only explicitly classified discovery
        # labels. No raw stderr is persisted or printed.
        pipe = self.daemon.stderr
        reasons = self.discovery_reasons
        def drain():
            with pipe:
                while True:
                    line = pipe.readline(4096)
                    if not line:
                        return
                    match = re.fullmatch(
                        rb"fleet-w2-discovery gentle-shell ([a-z_]+)\r?\n", line)
                    if match and len(reasons) < 32:
                        reasons.append(match.group(1).decode("ascii"))
        threading.Thread(target=drain, daemon=True).start()
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            require(self.daemon.poll() is None, "fixture daemon exited")
            if (self.root / "runtime" / "fleetd.addr").is_file():
                try:
                    self.cli("workspace", "list", timeout=3)
                    require(self.daemon.poll() is None, "fixture daemon exited after readiness")
                    return
                except Failure:
                    pass  # Read-only readiness check; never retry a mutation.
            time.sleep(.2)
        raise Failure("fixture daemon readiness timed out")

    def stop_daemon(self):
        if self.daemon is not None and self.daemon.poll() is None:
            self.daemon.terminate()
            self.daemon.wait(timeout=10)

    def timeline(self):
        events = []
        while True:
            page = jsonl(self.cli("timeline", self.session, "--since", str(len(events)),
                                  "--limit", "256", "--json", timeout=20))
            require(len(page) <= 256, "invalid timeline page")
            for event in page:
                seq = len(events) + 1
                require(event["seq"] == seq and event["session_id"] == self.session
                        and event["event_id"] == f"{self.session}-e{seq}",
                        "timeline sequence/identity mismatch")
                events.append(event)
            require(len(events) <= 8192, "timeline exceeds smoke limit")
            if len(page) < 256:
                return events

    def replay(self, events):
        replay = jsonl(self.cli("watch", self.session, "--since", "0", "--count",
                               str(len(events)), "--json", timeout=45))
        require(replay == events, "durable replay differs from timeline")

    def operation(self, key, operation_id):
        deadline = time.monotonic() + 10
        while True:
            row = self.cli("operation", key, timeout=10).split("\t", 4)
            require(len(row) >= 4 and row[:2] == [operation_id, key],
                    "operation identity mismatch")
            if row[3] == "Completed":
                return
            require(row[3] not in ("Failed", "Uncertain"), "operation failed or uncertain")
            require(time.monotonic() < deadline, "operation completion not observed")
            time.sleep(.2)

    def prompt(self, message):
        key = uuid.uuid4().hex
        self.env["FLEET_OPERATION_KEY"] = key
        receipt = self.cli("prompt", self.session, message).split("\t")
        require(len(receipt) == 3 and receipt[1] == self.session and receipt[2],
                "prompt receipt mismatch")
        return key, receipt[0], receipt[2]

    def wait_turn(self, turn, allowed_tools):
        deadline = time.monotonic() + self.args.turn_timeout
        while True:
            events = self.timeline()
            kinds = [e["kind"] for e in events if e["kind"].get("turn_id") == turn]
            if any(k["type"] == "approval_requested" for k in kinds):
                raise Blocked("native approval requested; not automatically answered")
            if not allowed_tools and any(k["type"] == "tool_started" for k in kinds):
                raise Failure("chat-only turn unexpectedly started a tool")
            if any(k["type"] == "turn_uncertain" for k in kinds):
                raise Failure("turn outcome uncertain; not retried")
            terminals = [k for k in kinds if k["type"] == "turn_completed"]
            if terminals:
                require(len(terminals) == 1, "duplicate terminal event")
                return events, kinds, terminals[0]["status"]
            require(time.monotonic() < deadline, "model turn did not complete")
            time.sleep(.4)

    def chat(self, tools=False):
        marker = "FLEET_" + uuid.uuid4().hex.upper()
        if tools:
            (self.workspace / "fixture.txt").write_text(marker, encoding="utf-8")
            message = ("Read fixture.txt in the current workspace using a read-only file tool. "
                       "Return its contents exactly. Do not write files or run any other task.")
        else:
            message = f"Reply with exactly {marker}. Do not use any tools."
        before = len(self.timeline())
        key, operation_id, turn = self.prompt(message)
        events, kinds, status = self.wait_turn(turn, tools)
        # AgentError/AgentNotice may be session-scoped rather than turn-scoped.
        # Include only events observed within this prompt window.
        context = [e["kind"] for e in events[before:]
                   if not e["kind"].get("turn_id")
                   and e["kind"]["type"] in ("agent_error", "agent_notice")]
        diagnostic = turn_diagnostics(kinds, marker, context)
        self.row.setdefault("turn_diagnostics", []).append(
            {"stage": "read-only tool" if tools else "chat", **diagnostic})
        require(status == "Completed", f"turn ended with {status}")
        require(sum(k["type"] == "user_message" and k["text"] == message for k in kinds) == 1,
                "user message missing or duplicated")
        require(diagnostic["delta_chars"] > 0,
                "no nonempty streamed text; final message alone does not prove streaming")
        require(diagnostic["marker_in_deltas"] or diagnostic["marker_in_messages"],
                "expected marker missing from assistant text")
        if tools:
            require(any(k["type"] == "tool_started" for k in kinds)
                    and any(k["type"] == "tool_completed" and k["success"] for k in kinds),
                    "successful tool lifecycle missing")
            require((self.workspace / "fixture.txt").read_text(encoding="utf-8") == marker,
                    "read-only fixture was modified")
        self.operation(key, operation_id)
        self.replay(events)
        return events

    def recovery(self, events):
        self.stop_daemon()
        # Only the isolated fixture's endpoint marker, after owner exit.
        (self.root / "runtime" / "fleetd.addr").unlink(missing_ok=True)
        self.start_daemon()
        observed = self.timeline()
        require(observed[:len(events)] == events, "timeline prefix changed after restart")
        self.replay(observed)
        rows = [line.split("\t") for line in self.cli("session", "list").splitlines()]
        require(any(len(r) >= 4 and r[0] == self.session and r[3] == "Running" for r in rows),
                "session not running after daemon restart")
        self.chat()

    def interrupt(self):
        # Native generation speed is nondeterministic. If the turn completes
        # before interrupt is accepted, report BLOCKED, not fake success.
        key, operation_id, turn = self.prompt(
            "Write a long numbered list of 500 short fictional names. "
            "Do not use tools or access any files.")
        deadline = time.monotonic() + self.args.turn_timeout
        while True:
            events = self.timeline()
            kinds = [e["kind"] for e in events if e["kind"].get("turn_id") == turn]
            if any(k["type"] == "approval_requested" for k in kinds):
                raise Blocked("native approval requested; not automatically answered")
            require(not any(k["type"] in ("tool_started", "turn_uncertain") for k in kinds),
                    "unexpected tool or uncertain turn during interrupt probe")
            terminals = [k for k in kinds if k["type"] == "turn_completed"]
            if terminals:
                if len(terminals) == 1 and terminals[0]["status"] == "Completed":
                    raise Blocked("turn completed before interrupt could be issued")
                raise Failure("unexpected terminal before interrupt")
            if any(k["type"] == "assistant_delta" for k in kinds):
                break
            require(time.monotonic() < deadline, "no streaming before interrupt deadline")
            time.sleep(.2)
        try:
            reply = self.cli("interrupt", self.session, turn, timeout=60)
        except Failure:
            events = self.timeline()
            if any(e["kind"].get("turn_id") == turn
                   and e["kind"]["type"] == "turn_completed"
                   and e["kind"]["status"] == "Completed" for e in events):
                raise Blocked("turn completed while interrupt was requested") from None
            raise
        require(reply == f"interrupted\t{self.session}\t{turn}", "interrupt receipt mismatch")
        events, _, status = self.wait_turn(turn, False)
        if status == "Completed":
            raise Blocked("completion won the race with interrupt")
        require(status == "Interrupted", "native interrupt terminal was not observed")
        operation = self.cli("operation", key).split("\t", 4)
        require(len(operation) >= 4 and operation[:2] == [operation_id, key],
                "interrupt operation identity mismatch")
        self.replay(events)

    def run(self):
        stage = "startup"
        try:
            for exe in EXES:
                shutil.copy2(ROOT / "target/debug" / exe, self.bin / exe)
            self.start_daemon()
            stage = "discovery"
            agents = json.loads(self.cli("agents", "--json", timeout=240))
            matches = [a for a in agents if a["id"] == self.provider]
            require(len(matches) == 1, "provider missing or duplicate in Fleet catalog")
            agent = matches[0]
            version = version_label(agent.get("version"))
            self.row["version"] = version
            self.row["mode"] = agent["mode"]
            self.row["availability"] = agent["availability"]
            if not compatible(self.provider, version, self.args.experimental_gentle_shell_5):
                if self.provider == "gentle-shell":
                    # Allow the stderr drain to consume the already written
                    # diagnostic before the unavailable result is recorded.
                    time.sleep(.1)
                    self.row["discovery_reasons"] = list(self.discovery_reasons)
                raise Blocked(f"installed version {version} outside validated range or undetected")
            owned_candidate = (self.provider == "opencode"
                               and self.args.opencode_mode == "owned")
            if agent["availability"] != "Available" and not owned_candidate:
                raise Blocked(f"provider not available; version {version}")
            self.mark(stage, detail=f"{version}, {agent['mode']}, {agent['availability']}")
            stage = "session"
            workspace = self.cli("workspace", "add", str(self.workspace)).split("\t", 1)[0]
            self.create_attempted = True
            receipt = self.cli("session", "create", "--workspace", workspace,
                               "--provider", self.provider, timeout=100).split("\t")
            require(len(receipt) == 4 and receipt[1:3] == [self.provider, "Running"],
                    "session creation not confirmed")
            self.session = receipt[0]
            self.mark(stage)
            if self.args.stop_only:
                self.row["status"] = "PASS"
                return
            stage = "chat"
            events = self.chat()
            self.mark(stage, detail="streaming, one terminal, operation and exact replay")
            stage = "controls"
            controls = json.loads(self.cli("controls", self.session, "--json"))
            require(isinstance(controls, list), "invalid controls response")
            self.mark(stage, detail=f"{len(controls)} controls read; no settings changed")
            if self.args.tools:
                stage = "read-only tool"
                events = self.chat(tools=True)
                self.mark(stage)
            if self.args.recovery:
                stage = "daemon recovery"
                self.recovery(events)
                self.mark(stage)
            if self.args.interrupt:
                stage = "interrupt"
                if not agent["capabilities"]["cancel_turn"]:
                    self.mark(stage, "UNSUPPORTED", "provider does not advertise cancellation")
                else:
                    self.interrupt()
                    self.mark(stage)
            self.row["status"] = "PASS"
        except Blocked as error:
            self.mark(stage, "BLOCKED", str(error))
            self.row["status"] = "BLOCKED"
        except Failure as error:
            self.mark(stage, "FAIL", str(error))
            self.row["status"] = "FAIL"
        except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
            self.mark(stage, "FAIL", type(error).__name__)
            self.row["status"] = "FAIL"
        finally:
            self.cleanup()

    def cleanup(self):
        if self.session:
            try:
                require(self.cli("stop", self.session) == "stopped\t" + self.session,
                        "stop receipt not confirmed")
                rows = [line.split("\t") for line in self.cli("session", "list").splitlines()]
                require(any(len(r) >= 4 and r[0] == self.session and r[3] == "Stopped"
                            for r in rows), "durable Stopped missing")
                self.stopped = True
                self.mark("stop")
            except (Failure, OSError, ValueError) as error:
                detail = str(error) if isinstance(error, Failure) else type(error).__name__
                self.mark("stop", "FAIL", detail + "; fixture retained")
                try:
                    events = self.timeline()
                    self.row["stop_diagnostics"] = {
                        "session_states": [e["kind"]["state"] for e in events
                                           if e["kind"]["type"] == "session_state_changed"],
                        "notice_reasons": [reason for e in events
                                           if e["kind"]["type"] == "agent_notice"
                                           for reason in [stop_reason(e["kind"]["message"])]
                                           if reason != "unknown"],
                    }
                except (Failure, OSError, ValueError, KeyError):
                    self.row["stop_diagnostics"] = {"status": "unavailable"}
                self.row["status"] = "FAIL"
        if self.create_attempted and not self.stopped:
            self.row["retained_fixture"] = str(self.root)
            print("Fixture retained for explicit reconciliation:", self.root, flush=True)
            return
        try:
            self.stop_daemon()
            if self.row["status"] != "PASS":
                # Preserve the failed turn's evidence even when stop succeeds.
                # No daemon remains running solely to retain diagnostics.
                self.row["retained_fixture"] = str(self.root)
                print("Stopped fixture retained for local diagnosis:", self.root, flush=True)
            else:
                shutil.rmtree(self.root)
        except (OSError, subprocess.TimeoutExpired):
            self.row["retained_fixture"] = str(self.root)
            self.row["status"] = "FAIL"
            self.mark("cleanup", "FAIL", "fixture cleanup not confirmed")


def inspect_fixture(root):
    """Read only the durable API of an earlier isolated fixture."""
    root = root.resolve()
    require(root.parent == Path(tempfile.gettempdir()).resolve()
            and root.name.startswith("fleet w2 ")
            and (root / "runtime" / "fleetd.addr").is_file(),
            "expected an existing W2 fixture runtime in TEMP")
    exe = ROOT / "target/debug/fleetctl.exe"
    require(exe.is_file(), "build fleetctl first")
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("FLEET_")}
    env.update(FLEET_RUNTIME_DIR=str(root / "runtime"),
               FLEET_DB_PATH=str(root / "private data" / "fleet.sqlite"),
               FLEET_OPERATION_JOURNAL=str(root / "private client" / "operations.jsonl"),
               FLEET_CURSOR_JOURNAL=str(root / "private client" / "cursors.jsonl"))

    def read_cli(*argv):
        result = subprocess.run([str(exe), *argv], env=env, cwd=ROOT,
                                capture_output=True, text=True, encoding="utf-8",
                                timeout=20)
        require(result.returncode == 0, f"read-only {argv[0]} failed")
        return result.stdout.rstrip("\r\n")

    rows = [row.split("\t") for row in read_cli("session", "list").splitlines()]
    summary = {"fixture": root.name, "sessions": []}
    for row in rows:
        require(len(row) == 5, "invalid session list row")
        session, provider, _, state, turn_state = row
        require(provider in PROVIDERS, "unexpected provider in W2 fixture")
        events = []
        while True:
            page = jsonl(read_cli("timeline", session, "--since", str(len(events)),
                                  "--limit", "256", "--json"))
            for event in page:
                require(event["seq"] == len(events) + 1 and event["session_id"] == session,
                        "invalid fixture timeline sequence")
                events.append(event)
            require(len(events) <= 8192, "fixture timeline exceeds limit")
            if len(page) < 256:
                break
        turns = {}
        for event in events:
            kind = event["kind"]
            if kind.get("turn_id"):
                turns.setdefault(kind["turn_id"], []).append(kind)
        turn_rows = []
        for kinds in turns.values():
            user = next((k["text"] for k in kinds if k["type"] == "user_message"), "")
            marker = re.search(r"\bFLEET_[0-9A-F]{32}\b", user)
            expected = marker.group(0) if marker else None
            if expected is None and user.startswith("Read fixture.txt"):
                try:
                    path = root / "workspace with spaces" / "fixture.txt"
                    if path.stat().st_size <= 128:
                        content = path.read_text(encoding="utf-8").strip()
                        if re.fullmatch(r"FLEET_[0-9A-F]{32}", content):
                            expected = content
                except (OSError, UnicodeError):
                    pass
            turn_rows.append(turn_diagnostics(kinds, expected))
        summary["sessions"].append({
            "provider": provider, "state": state, "turn_state": turn_state,
            "turns": turn_rows,
            "session_error_classes": error_classes(
                [e["kind"] for e in events if not e["kind"].get("turn_id")]),
            "notice_reasons": [reason for e in events if e["kind"]["type"] == "agent_notice"
                               for reason in [stop_reason(e["kind"]["message"])]
                               if reason != "unknown"],
        })
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inspect-fixture", type=Path, action="append",
                        help="read-only diagnostic of a retained W2 fixture; no model call")
    parser.add_argument("--real", action="store_true", help="authorize real model turns")
    parser.add_argument("--provider", choices=("all", *PROVIDERS), default="all")
    parser.add_argument("--experimental-gentle-shell-5", action="store_true",
                        help="explicitly test exact 5.0.0 candidate; not release support")
    parser.add_argument("--stop-only", action="store_true", help="open/close session without a model turn")
    parser.add_argument("--tools", action="store_true", help="extra read-only tool turn")
    parser.add_argument("--recovery", action="store_true", help="extra chat after daemon restart")
    parser.add_argument("--interrupt", action="store_true", help="extra streaming interruption turn")
    parser.add_argument("--turn-timeout", type=int, default=180)
    parser.add_argument("--opencode-mode", choices=("auto", "attached", "owned"), default="attached")
    for option in ("opencode-bin", "codex-bin", "pi-bin", "pi-js",
                   "gentle-shell-bin", "gentle-shell-js", "node-bin"):
        parser.add_argument("--" + option, type=Path)
    args = parser.parse_args()
    if args.inspect_fixture:
        if os.name != "nt":
            print("BLOCKED: fixture inspection requires Windows")
            return 1
        rows = []
        failed = False
        for root in args.inspect_fixture:
            try:
                rows.append(inspect_fixture(root))
            except (Failure, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
                rows.append({"fixture": root.name, "status": "BLOCKED",
                             "detail": str(error) if isinstance(error, Failure) else type(error).__name__})
                failed = True
        report_dir = ROOT / "reports/w2"
        report_dir.mkdir(parents=True, exist_ok=True)
        report_path = report_dir / ("inspection-" + uuid.uuid4().hex + ".json")
        report_path.write_text(json.dumps(rows, indent=2), encoding="utf-8")
        print(json.dumps(rows, indent=2))
        print("Report:", report_path)
        return int(failed)
    if os.name != "nt" or not args.real:
        print("BLOCKED: requires native Windows and explicit --real")
        return 1
    if args.stop_only and (args.tools or args.recovery or args.interrupt):
        parser.error("--stop-only cannot be combined with model-turn options")
    if not 1 <= args.turn_timeout <= 600:
        parser.error("--turn-timeout must be 1..600 seconds")
    if not all((ROOT / "target/debug" / exe).is_file() for exe in EXES):
        print("BLOCKED: build fleetd, fleet-runner and fleetctl first")
        return 1
    launch = discover_paths(args)
    for key, value in launch.items():
        path = Path(value)
        extensions = (".js", ".mjs", ".cjs") if key.endswith("_JS") else (".exe",)
        if not path.is_absolute() or not path.is_file() or path.suffix.lower() not in extensions:
            print(f"BLOCKED: invalid explicit launch path for {key}")
            return 1
    for prefix in ("FLEET_PI", "FLEET_GENTLE_SHELL"):
        if prefix + "_BIN" in launch and prefix + "_JS" in launch:
            print(f"BLOCKED: choose exactly one launcher for {prefix}")
            return 1
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True)
    if dirty:
        print("BLOCKED: checkout dirty")
        return 1
    report_dir = ROOT / "reports/w2"
    report_dir.mkdir(parents=True, exist_ok=True)
    report_path = report_dir / (uuid.uuid4().hex + ".json")
    report = {"commit": sha, "started": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "experimental_gentle_shell_5": args.experimental_gentle_shell_5,
              "stop_only": args.stop_only, "tools": args.tools,
              "recovery": args.recovery, "interrupt": args.interrupt,
              "opencode_mode": args.opencode_mode, "providers": []}
    for provider in PROVIDERS if args.provider == "all" else (args.provider,):
        row = {"provider": provider, "status": "BLOCKED", "checks": []}
        report["providers"].append(row)
        Fixture(provider, launch, args, row).run()
        report_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print("\nSummary:")
    for row in report["providers"]:
        print(f"{row['provider']}: {row['status']} ({row.get('version', 'unknown')})")
    print("Report:", report_path)
    return 0 if all(row["status"] == "PASS" for row in report["providers"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
