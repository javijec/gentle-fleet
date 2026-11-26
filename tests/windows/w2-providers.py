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


def compatible(provider, version):
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
        "FLEET_GENTLE_SHELL_JS": Path(os.environ.get("APPDATA", str(home))) /
            "npm/node_modules/gentle-pi/bin/gentle-shell.mjs",
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
        )
        self.daemon = None
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
            raise Failure(f"CLI {argv[0]} exit {result.returncode}; code={code}")
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
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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
        key, operation_id, turn = self.prompt(message)
        events, kinds, status = self.wait_turn(turn, tools)
        require(status == "Completed", f"turn ended with {status}")
        require(sum(k["type"] == "user_message" and k["text"] == message for k in kinds) == 1,
                "user message missing or duplicated")
        deltas = [k["text"] for k in kinds if k["type"] == "assistant_delta"]
        messages = [k["text"] for k in kinds if k["type"] == "assistant_message"]
        require(bool(deltas) and marker in ("".join(deltas) + "".join(messages)),
                "expected streamed answer missing")
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
            if not compatible(self.provider, version):
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
            except (Failure, OSError, ValueError):
                self.mark("stop", "FAIL", "stop not confirmed; fixture retained")
                self.row["status"] = "FAIL"
        if self.create_attempted and not self.stopped:
            self.row["retained_fixture"] = str(self.root)
            print("Fixture retained for explicit reconciliation:", self.root, flush=True)
            return
        try:
            self.stop_daemon()
            shutil.rmtree(self.root)
        except (OSError, subprocess.TimeoutExpired):
            self.row["retained_fixture"] = str(self.root)
            self.row["status"] = "FAIL"
            self.mark("cleanup", "FAIL", "fixture cleanup not confirmed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--real", action="store_true", help="authorize real model turns")
    parser.add_argument("--provider", choices=("all", *PROVIDERS), default="all")
    parser.add_argument("--tools", action="store_true", help="extra read-only tool turn")
    parser.add_argument("--recovery", action="store_true", help="extra chat after daemon restart")
    parser.add_argument("--interrupt", action="store_true", help="extra streaming interruption turn")
    parser.add_argument("--turn-timeout", type=int, default=180)
    parser.add_argument("--opencode-mode", choices=("auto", "attached", "owned"), default="attached")
    for option in ("opencode-bin", "codex-bin", "pi-bin", "pi-js",
                   "gentle-shell-bin", "gentle-shell-js", "node-bin"):
        parser.add_argument("--" + option, type=Path)
    args = parser.parse_args()
    if os.name != "nt" or not args.real:
        print("BLOCKED: requires native Windows and explicit --real")
        return 1
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
              "tools": args.tools, "recovery": args.recovery, "interrupt": args.interrupt,
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
