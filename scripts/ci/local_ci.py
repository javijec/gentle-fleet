#!/usr/bin/env python3
"""Local deterministic CI. Python 3.10+, no external Python dependencies."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
PROFILES = ("quick", "full", "release")
PLATFORMS = ("windows", "linux")
E2E = (
    "e2e_fake", "e2e_concurrency", "e2e_backpressure", "e2e_unix_permissions",
    "e2e_persistence", "e2e_approvals", "e2e_p3_survival",
    "e2e_p3_approval_disconnect", "e2e_p3_identity", "e2e_p3_provider_health",
    "e2e_p3_runner_kill", "e2e_p3_multi_session", "e2e_long_replay",
    "e2e_p44_conformity", "e2e_p44_concurrency", "e2e_p44_security",
    "e2e_p44_redaction", "e2e_p44_approvals", "e2e_p51_ssh",
    "e2e_p52_two_hosts", "e2e_p61_sdk",
)
FAKE_ONLY = {"e2e_p44_conformity", "e2e_p44_concurrency", "e2e_p44_security"}
MANUAL = {
    "windows": ("native-providers", "cross-user-ipc-acl", "runtime-acl-delete-child",
                "ipc-backpressure-cancel-peer-identity", "native-recovery",
                "physical-ssh", "external-sdk-smoke", "dependency-audit"),
    "linux": ("native-providers", "physical-ssh", "external-sdk-smoke", "dependency-audit"),
}


def current_platform():
    name = platform.system().lower()
    if name not in PLATFORMS:
        raise RuntimeError("Only native Windows and Linux are supported")
    return name


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def clean_head(expected=None):
    sha = git("rev-parse", "HEAD")
    if expected and sha != expected:
        raise RuntimeError(f"HEAD {sha} differs from requested {expected}")
    if git("status", "--porcelain", "--untracked-files=normal"):
        raise RuntimeError("Checkout is dirty; commit/stash changes or use a clean worktree")
    return sha


def step(name, argv, depends=(), requires=(), env=None):
    return dict(id=name, argv=argv, depends=depends, requires=requires, env=env or {})


def plan(os_name, profile):
    if os_name not in PLATFORMS or profile not in PROFILES:
        raise ValueError("Invalid platform/profile")
    items = [
        step("headless-graph", [sys.executable, str(ROOT / "scripts/ci/check_headless.py")]),
        step("format", ["cargo", "fmt", "--all", "--", "--check"]),
        step("check", ["cargo", "check", "--workspace", "--locked"]),
        step("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]),
        step("ci-selftest", [sys.executable, str(ROOT / "scripts/ci/test_local_ci.py")]),
        step("ci-both-selftest", [sys.executable, str(ROOT / "scripts/ci/test_both.py")]),
    ]
    if profile == "quick":
        return items
    items += [
        step("diff-check", ["git", "diff", "--check", "HEAD^", "HEAD"]),
        step("test", ["cargo", "test", "--workspace", "--locked"]),
        step("build", ["cargo", "build", "--workspace", "--locked"]),
    ]
    if os_name == "windows":
        items += [
            step("ipc-test", ["cargo", "test", "-p", "fleet-protocol", "--locked"]),
            step("ipc-build", ["cargo", "build", "-p", "fleetd", "-p", "fleet-runner", "-p", "fleetctl", "-p", "fleet-ssh", "--locked"]),
            step("w1a-token-isolation", ["pwsh", "-NoProfile", "-File", "tests/windows/w1a-token-isolation.ps1"], ("build", "ipc-build")),
            step("w1b-named-pipe", ["pwsh", "-NoProfile", "-File", "tests/windows/w1b-named-pipe.ps1"], ("build", "ipc-build")),
        ]
    else:
        for script in sorted((ROOT / "tests").glob("*.sh")):
            items.append(step("syntax-" + script.stem, ["bash", "-n", str(script)]))
        for name in E2E:
            req = ("sshd",) if name in {"e2e_p51_ssh", "e2e_p52_two_hosts"} else ()
            env = {"GF_E2E_FAKE_ONLY": "1"} if name in FAKE_ONLY else {}
            items.append(step(name, ["bash", "tests/" + name + ".sh"], ("build",), req, env))
    return items


def timestamp():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with tmp.open("w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")
    tmp.replace(path)


def overall(rows):
    states = {r["status"] for r in rows}
    if not rows or not states <= {"PASS", "FAIL", "SKIP", "BLOCKED"}:
        return "BLOCKED"
    if "FAIL" in states:
        return "FAIL"
    if states != {"PASS"}:
        return "BLOCKED"
    return "PASS"


def run_step(item, results, logs):
    name = item["id"]
    row = {"id": name, "command": item["argv"], "status": "BLOCKED"}
    missing = [x for x in item["depends"] if results.get(x) != "PASS"]
    commands = (item["argv"][0], *item["requires"])
    missing_tools = [x for x in commands if shutil.which(x) is None]
    if missing or missing_tools:
        row["reason"] = f"Dependencies not passed: {missing}; missing tools: {missing_tools}"
        return row
    env = os.environ.copy()
    for key in ("GF_E2E_REAL", "GF_E2E_FAKE_ONLY"):
        env.pop(key, None)
    env.update(item["env"])
    # Only deterministic fixtures here. Logs are local, never uploaded automatically.
    path = logs / (name + ".log")
    start = time.monotonic()
    print(f"RUN {name} (log: {path})", flush=True)
    try:
        with path.open("xb") as f:
            if os.name != "nt":
                os.chmod(path, 0o600)
            code = subprocess.run(item["argv"], cwd=ROOT, env=env, stdout=f, stderr=subprocess.STDOUT).returncode
        row.update(exit_code=code, log=str(path), status="PASS" if code == 0 else "FAIL")
        if code == 0:
            # A fixture can exit zero after printing SKIP/BLOCKED or fail markers.
            with path.open(encoding="utf-8", errors="replace") as f:
                for line in f:
                    match = re.match(r"^\s*(SKIP|BLOCKED|FAIL)\b", line)
                    if match:
                        state = match.group(1)
                        if state == "FAIL" or row["status"] == "PASS":
                            row["status"] = state
    except OSError as exc:
        row["reason"] = str(exc)
    row["seconds"] = round(time.monotonic() - start, 3)
    return row


def tool_version(name):
    try:
        return subprocess.check_output([name, "--version"], cwd=ROOT, text=True, stderr=subprocess.STDOUT).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def manual_rows(os_name, sha, evidence):
    records = {}
    if evidence:
        payload = json.loads(evidence.read_text(encoding="utf-8"))
        if payload.get("commit") != sha or payload.get("platform") != os_name:
            raise RuntimeError("Manual evidence commit/platform mismatch")
        records = payload.get("checks", {})
    rows = []
    for name in MANUAL[os_name]:
        entry = records.get(name, {})
        state = entry.get("status", "BLOCKED")
        note = entry.get("evidence", "")
        if state not in {"PASS", "FAIL", "BLOCKED", "SKIP"} or not isinstance(note, str) or not note.strip():
            state, note = "BLOCKED", "Manual evidence not provided"
        rows.append({"id": "manual-" + name, "status": state, "evidence": note, "source": "operator"})
    return rows


def run(args):
    os_name = current_platform()
    sha = clean_head(args.expect_sha)
    out = args.output or ROOT / "reports" / f"{os_name}-{args.profile}.json"
    logs = ROOT / "reports" / "logs" / (os_name + "-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ"))
    logs.mkdir(parents=True, mode=0o700)
    rows = []
    data = dict(schema=1, source="local", platform=os_name, os=platform.platform(),
                architecture=platform.machine(), commit=sha, profile=args.profile,
                started=timestamp(), rustc=tool_version("rustc"), cargo=tool_version("cargo"),
                checks=rows, status="BLOCKED")
    # Invalidate an old report immediately, including if this run is interrupted.
    write_json(out, data)
    try:
        results = {}
        for item in plan(os_name, args.profile):
            row = run_step(item, results, logs)
            rows.append(row)
            results[item["id"]] = row["status"]
            print(f'{row["status"]}: {item["id"]}', flush=True)
            write_json(out, data)
        if args.profile == "release":
            rows.extend(manual_rows(os_name, sha, args.manual_evidence))
        clean_head(sha)
        data["status"] = overall(rows)
    finally:
        data["finished"] = timestamp()
        write_json(out, data)
    print(f'{data["status"]}: {out}', flush=True)
    return 0 if data["status"] == "PASS" else 1


def expected_ids(os_name, profile):
    ids = {x["id"] for x in plan(os_name, profile)}
    if profile == "release":
        ids |= {"manual-" + x for x in MANUAL[os_name]}
    return ids


def validate_report(data, os_name, sha, profile):
    if (data.get("schema"), data.get("source"), data.get("platform"),
        data.get("commit"), data.get("profile")) != (1, "local", os_name, sha, profile):
        raise ValueError("Report metadata mismatch")
    rows = data["checks"]
    ids = [x["id"] for x in rows]
    # Shell syntax rows are generated from the same clean checkout on both hosts.
    if any(x.get("status") not in {"PASS", "FAIL", "SKIP", "BLOCKED"} for x in rows):
        raise ValueError("Unknown check status")
    if len(ids) != len(set(ids)) or set(ids) != expected_ids(os_name, profile):
        raise ValueError("Missing, duplicate or unexpected checks")
    status = overall(rows)
    if data.get("status") != status or not data.get("finished"):
        raise ValueError("Incomplete/inconsistent report")
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=PROFILES, default="full")
    parser.add_argument("--expect-sha")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--manual-evidence", type=Path)
    args = parser.parse_args()
    if args.manual_evidence and args.profile != "release":
        parser.error("--manual-evidence requires release")
    try:
        return run(args)
    except (RuntimeError, OSError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"BLOCKED: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
