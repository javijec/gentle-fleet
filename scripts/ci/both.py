#!/usr/bin/env python3
"""Interactive native Windows/Linux CI coordinator (Python 3.10+)."""
import argparse
import base64
import json
import os
from pathlib import PurePosixPath, PureWindowsPath
import re
import subprocess
import sys
import uuid

from local_ci import ROOT, clean_head, current_platform, git, run_step, timestamp, validate_report, write_json
from console import color_enabled, status_text, summary

SSH_OPTIONS = ["-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
               "-o", "ConnectTimeout=15"]


def encoded_command(source):
    encoded = base64.b64encode(source.encode("utf-8")).decode("ascii")
    return " -c \"import base64;exec(base64.b64decode('" + encoded + "'))\""


def remote_command(root, arguments, sha):
    # The only variable material interpreted by the remote shell is base64.
    # Works with the usual cmd.exe, PowerShell and POSIX SSH default shells.
    source = (
        "import os, subprocess, sys\n"
        f"os.chdir({root!r})\n"
        "head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()\n"
        f"if head != {sha!r}: sys.exit('BLOCKED: remote HEAD mismatch; update checkout first')\n"
        "if subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=normal'], text=True).strip():\n"
        "    sys.exit('BLOCKED: remote checkout dirty')\n"
        f"sys.exit(subprocess.call([sys.executable, 'scripts/ci/both.py', *{arguments!r}]))\n"
    )
    return encoded_command(source)


def invoke(host, root, platform, args, sha, capture=False):
    # Forward ANSI only for human-readable worker output, never JSON collection.
    if not capture and color_enabled():
        args = [*args, "--color-output"]
    if host == "local":
        cmd = [sys.executable, str(ROOT / "scripts/ci/both.py"), *args]
    else:
        python = "python" if platform == "windows" else "python3"
        cmd = ["ssh", *SSH_OPTIONS, host, python + remote_command(root, args, sha)]
    return subprocess.run(cmd, cwd=ROOT, text=True, encoding="utf-8",
                          errors="replace", stdout=subprocess.PIPE if capture else None)


def worker(args):
    if args.color_output:
        os.environ["FLEET_CI_COLOR"] = "1"
    else:
        os.environ.pop("FLEET_CI_COLOR", None)
    if current_platform() != args.platform:
        raise RuntimeError("Native platform mismatch")
    clean_head(args.sha)
    if not re.fullmatch(r"[0-9a-f]{32}", args.run_id):
        raise ValueError("Invalid run ID")
    folder = ROOT / "reports" / "both" / args.run_id
    if args.worker == "preflight":
        print(f"{status_text('READY')}: {args.platform} {args.sha[:12]}", flush=True)
        return 0
    if args.worker == "collect":
        # ASCII JSON survives the remote Windows console code page.
        print(json.dumps(json.loads((folder / "native.json").read_text(encoding="utf-8")),
                         ensure_ascii=True))
        return 0
    folder.mkdir(parents=True, exist_ok=False)
    profile = "full" if args.mode in ("w1c", "w1f") else args.mode
    report_path = folder / "ci.json"
    record = {"platform": args.platform, "commit": args.sha, "mode": args.mode,
              "started": timestamp(), "status": "BLOCKED", "protocol_runs": [],
              "lifecycle_runs": []}
    native = folder / "native.json"
    write_json(native, record)
    try:
        # Preserve the default Rust test parallelism for the repeated IPC tests.
        os.environ.pop("RUST_TEST_THREADS", None)
        code = subprocess.run(
            [sys.executable, str(ROOT / "scripts/ci/local_ci.py"), "--profile", profile,
             "--expect-sha", args.sha, "--output", str(report_path)], cwd=ROOT,
        ).returncode
        record["ci_exit_code"] = code
        record["ci"] = json.loads(report_path.read_text(encoding="utf-8"))
        status = validate_report(record["ci"], args.platform, args.sha, profile)
        if code != 0 or status != "PASS":
            record["status"] = "FAIL" if status == "FAIL" else "BLOCKED"
            return 1
        if args.mode in ("w1c", "w1f") and args.platform == "windows":
            for index in range(1, 11):
                item = {"id": f"protocol-{index:02d}",
                        "argv": ["cargo", "test", "-p", "fleet-protocol", "--locked"],
                        "depends": (), "requires": (), "env": {}}
                row = run_step(item, {}, folder)
                record["protocol_runs"].append(row)
                write_json(native, record)
                if row["status"] != "PASS":
                    record["status"] = "FAIL" if row["status"] == "FAIL" else "BLOCKED"
                    return 1
                clean_head(args.sha)
        if args.mode == "w1f" and args.platform == "windows":
            for index in range(1, 21):
                item = {"id": f"lifecycle-{index:02d}",
                        "argv": ["pwsh", "-NoProfile", "-File",
                                 "tests/windows/w1d-lifecycle.ps1", "-Scenario", "Lifecycle"],
                        "depends": (), "requires": (), "env": {}}
                row = run_step(item, {}, folder)
                record["lifecycle_runs"].append(row)
                write_json(native, record)
                if row["status"] != "PASS":
                    record["status"] = "FAIL" if row["status"] == "FAIL" else "BLOCKED"
                    return 1
                clean_head(args.sha)
        clean_head(args.sha)
        record["status"] = "PASS"
        return 0
    finally:
        record["finished"] = timestamp()
        write_json(native, record)
        if record["status"] != "PASS" and not record.get("ci"):
            summary(args.platform.upper(), record["status"], report=native)


def validate_native(record, platform, sha, mode):
    if (record.get("platform"), record.get("commit"), record.get("mode")) != (platform, sha, mode):
        raise ValueError("Native report identity mismatch")
    if record.get("status") not in ("PASS", "FAIL", "BLOCKED") or not record.get("finished"):
        raise ValueError("Incomplete native report")
    if record["status"] != "PASS":
        return record["status"]
    profile = "full" if mode in ("w1c", "w1f") else mode
    if record.get("ci_exit_code") != 0 or validate_report(record["ci"], platform, sha, profile) != "PASS":
        raise ValueError("Native CI did not pass")
    runs = record["protocol_runs"]
    expected = [f"protocol-{i:02d}" for i in range(1, 11)] if mode in ("w1c", "w1f") and platform == "windows" else []
    if [r["id"] for r in runs] != expected or any(
        r.get("status") != "PASS" or r.get("exit_code") != 0 for r in runs
    ):
        raise ValueError("Missing or failed protocol repetitions")
    runs = record.get("lifecycle_runs", [])
    expected = [f"lifecycle-{i:02d}" for i in range(1, 21)] if mode == "w1f" and platform == "windows" else []
    if [r["id"] for r in runs] != expected or any(
        r.get("status") != "PASS" or r.get("exit_code") != 0 for r in runs
    ):
        raise ValueError("Missing or failed lifecycle repetitions")
    return "PASS"


def ask(label, value, default=None):
    if value is not None:
        return value.strip()
    answer = input(label + (f" [{default}]" if default else "") + ": ").strip()
    return answer or default or ""


def git_run(*args):
    subprocess.run(["git", *args], cwd=ROOT, check=True)


def rust_diff():
    # -z is essential for filenames containing spaces or quoting characters.
    # Only tracked, unstaged modifications to Rust sources may be committed.
    raw = subprocess.check_output(
        ["git", "status", "--porcelain=v1", "-z", "--untracked-files=all"], cwd=ROOT
    )
    entries = raw.split(b"\0")
    if entries[-1] != b"" or any(
        not entry.startswith(b" M ") or not entry.endswith(b".rs")
        for entry in entries[:-1]
    ):
        raise RuntimeError("Unexpected changes after cargo fmt; review manually")
    return subprocess.check_output(["git", "diff", "--binary", "HEAD", "--"], cwd=ROOT)


def prepare_format():
    sha = clean_head()
    branch = git("symbolic-ref", "--short", "HEAD")
    ref = "refs/heads/" + branch
    # Explicit ref also works with an old single-branch fetch configuration.
    git_run("fetch", "--no-tags", "origin", ref)
    if git("rev-parse", "FETCH_HEAD") != sha:
        raise RuntimeError("Local branch is not at origin HEAD; pull --ff-only first")
    clean_head(sha)
    subprocess.run(["cargo", "fmt", "--all"], cwd=ROOT, check=True)
    diff = rust_diff()
    if not diff:
        clean_head(sha)
        print(f"{status_text('PASS')}: format already clean", flush=True)
        return branch, sha
    git_run("--no-pager", "diff", "HEAD", "--")
    print(f"Format changes will be committed and pushed to origin/{branch}.", flush=True)
    if input("Commit and push these formatting changes? [y/N]: ").strip().lower() not in ("y", "yes", "s", "si", "sí"):
        raise RuntimeError("Formatting changes left uncommitted; no tests started")
    if git("rev-parse", "HEAD") != sha or git("symbolic-ref", "--short", "HEAD") != branch or rust_diff() != diff:
        raise RuntimeError("Checkout changed during confirmation; review manually")
    git_run("add", "-u", "--", "*.rs")
    # Verify the staged patch matches the reviewed patch byte for byte.
    staged = subprocess.check_output(["git", "diff", "--cached", "--binary", "HEAD", "--"], cwd=ROOT)
    if staged != diff:
        raise RuntimeError("Staged changes differ from reviewed formatting")
    git_run("commit", "-m", "style: apply rustfmt")
    new_sha = clean_head()
    if git("rev-parse", "HEAD^") != sha or git("symbolic-ref", "--short", "HEAD") != branch:
        raise RuntimeError("Unexpected commit parent or branch; no push")
    committed = subprocess.check_output(["git", "diff", "--binary", sha, new_sha, "--"], cwd=ROOT)
    if committed != diff:
        raise RuntimeError("Committed changes differ from reviewed formatting; no push")
    git_run("push", "origin", "HEAD:" + ref)
    print(f"{status_text('PASS')}: format published {new_sha[:12]}", flush=True)
    return branch, new_sha


def sync_command(root, branch, sha):
    # Bootstrap independently of the remote version of both.py. Never reset,
    # stash, clean or force checkout. Preserve the remote's local branches.
    source = (
        "import os, subprocess, sys\n"
        f"os.chdir({root!r})\n"
        "def git(*args):\n"
        "    return subprocess.check_output(['git', *args], text=True).strip()\n"
        "def run(*args):\n"
        "    subprocess.run(['git', *args], check=True)\n"
        "if git('status', '--porcelain', '--untracked-files=normal'):\n"
        "    sys.exit('BLOCKED: remote checkout dirty')\n"
        f"run('fetch', '--no-tags', 'origin', {'refs/heads/' + branch!r})\n"
        f"if git('rev-parse', 'FETCH_HEAD') != {sha!r}:\n"
        "    sys.exit('BLOCKED: remote origin branch differs from tested SHA')\n"
        f"run('merge-base', '--is-ancestor', 'HEAD', {sha!r})\n"
        "if git('status', '--porcelain', '--untracked-files=normal'):\n"
        "    sys.exit('BLOCKED: remote checkout changed during fetch')\n"
        f"run('checkout', '--detach', '--no-overwrite-ignore', {sha!r})\n"
        f"if git('rev-parse', 'HEAD') != {sha!r} or git('status', '--porcelain', '--untracked-files=normal'):\n"
        "    sys.exit('BLOCKED: remote sync did not produce clean expected HEAD')\n"
    )
    return encoded_command(source)


def sync_remote(host, root, platform, branch, sha):
    python = "python" if platform == "windows" else "python3"
    subprocess.run(
        ["ssh", *SSH_OPTIONS, host, python + sync_command(root, branch, sha)],
        cwd=ROOT, check=True,
    )
    print(f"{status_text('PASS')}: {platform} synced {sha[:12]}", flush=True)


def coordinator(args):
    sha = clean_head()
    local = current_platform()
    mode = ask("Pruebas (quick/full/w1c/w1f)", args.mode, "w1c")
    if mode not in ("quick", "full", "w1c", "w1f"):
        raise ValueError("Use quick, full, w1c or w1f")
    hosts = {}
    for platform in ("windows", "linux"):
        host = ask(f"Alias SSH de {platform} (o local)", getattr(args, platform),
                   "local" if platform == local else None)
        if host == "local":
            if platform != local:
                raise ValueError("Local must match the native OS")
            root = str(ROOT)
        else:
            if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.@-]*", host):
                raise ValueError("Use an existing SSH alias or user@host")
            root = ask(f"Ruta del repositorio en {platform}", getattr(args, platform + "_root"))
            # Python resolves the path on the target OS, not on the coordinator.
            pure = PureWindowsPath(root) if platform == "windows" else PurePosixPath(root)
            if not pure.is_absolute() or ".." in pure.parts:
                raise ValueError("Use an absolute checkout path without parent traversal")
        hosts[platform] = (host, root)
    if getattr(args, "auto_format", False):
        if not any(host == "local" for host, _ in hosts.values()):
            raise ValueError("--auto-format requires the coordinator to be a local test host")
        branch, sha = prepare_format()
        for platform, (host, root) in hosts.items():
            if host != "local":
                sync_remote(host, root, platform, branch, sha)
        clean_head(sha)
    run_id = uuid.uuid4().hex
    # The local worker owns the run folder on this machine if selected.
    # Keep coordinator reports in a separate directory.
    summary_dir = ROOT / "reports" / "combined" / run_id
    summary_dir.mkdir(parents=True, exist_ok=False)
    data = {"commit": sha, "mode": mode, "started": timestamp(),
            "status": "BLOCKED", "platforms": {}, "manual_review": "REQUIRED"}
    output = summary_dir / "combined.json"
    write_json(output, data)
    try:
        for platform, (host, root) in hosts.items():
            common = ["--platform", platform, "--sha", sha, "--run-id", run_id, "--mode", mode]
            row = {"status": "BLOCKED"}
            data["platforms"][platform] = row
            try:
                if invoke(host, root, platform, ["--worker", "preflight", *common], sha).returncode != 0:
                    row["reason"] = "Preflight failed: verify SSH, Python, native OS and clean matching HEAD"
                    continue
                print(f"=== {platform.upper()} / {mode} / {sha[:12]} ===", flush=True)
                result = invoke(host, root, platform, ["--worker", "run", *common], sha)
                collected = invoke(host, root, platform, ["--worker", "collect", *common], sha, capture=True)
                if collected.returncode != 0:
                    row["reason"] = "Native report unavailable"
                    continue
                record = json.loads(collected.stdout)
                status = validate_native(record, platform, sha, mode)
                write_json(summary_dir / f"{platform}.json", record)
                checks = (record.get("ci", {}).get("checks", []) + record.get("protocol_runs", [])
                          + record.get("lifecycle_runs", []))
                counts = {key: sum(check["status"] == key for check in checks)
                          for key in ("PASS", "FAIL", "BLOCKED", "SKIP")}
                row.update(status=status, report=f"{platform}.json", exit_code=result.returncode,
                           counts=counts)
                if result.returncode != 0 and status == "PASS":
                    row.update(status="BLOCKED", reason="Worker exit code disagrees with report")
            except (OSError, ValueError, RuntimeError, KeyError, subprocess.CalledProcessError) as exc:
                row.update(status="BLOCKED", reason=str(exc))
            finally:
                write_json(output, data)
        clean_head(sha)
        states = {r["status"] for r in data["platforms"].values()}
        if len(data["platforms"]) == 2 and states == {"PASS"}:
            data["status"] = "AUTOMATED_PASS" if mode in ("w1c", "w1f") else "PASS"
        elif "FAIL" in states:
            data["status"] = "FAIL"
        return 0 if data["status"] in ("PASS", "AUTOMATED_PASS") else 1
    finally:
        data["finished"] = timestamp()
        write_json(output, data)
        print("\n=== SUMMARY ===", flush=True)
        for platform, row in data["platforms"].items():
            counts = row.get("counts", {})
            suffix = " | " + " | ".join(f"{status_text(key)} {value}"
                                         for key, value in counts.items()) if counts else ""
            print(f"{platform.upper():7s} {status_text(row['status'])}{suffix}", flush=True)
            if row.get("reason"):
                print(f"  Reason: {row['reason']}", flush=True)
        summary("TOTAL", data["status"], report=output)
        print("Review native logs; no manual or release gate is certified.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("quick", "full", "w1c", "w1f"))
    parser.add_argument("--auto-format", action="store_true",
                        help="Format clean local branch, confirm commit/push and sync SSH checkout")
    for platform in ("windows", "linux"):
        parser.add_argument("--" + platform, help="SSH alias or local")
        parser.add_argument("--" + platform + "-root", help="Absolute remote checkout path")
    parser.add_argument("--worker", choices=("preflight", "run", "collect"), help=argparse.SUPPRESS)
    parser.add_argument("--platform", choices=("windows", "linux"), help=argparse.SUPPRESS)
    parser.add_argument("--sha", help=argparse.SUPPRESS)
    parser.add_argument("--run-id", help=argparse.SUPPRESS)
    parser.add_argument("--color-output", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.worker:
            if args.auto_format:
                parser.error("--auto-format is only for the coordinator")
            if not all((args.platform, args.sha, args.run_id, args.mode)):
                parser.error("Incomplete worker arguments")
            return worker(args)
        return coordinator(args)
    except (OSError, RuntimeError, ValueError, KeyError, EOFError, subprocess.CalledProcessError) as exc:
        print(f"{status_text('BLOCKED')}: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("BLOCKED: interrupted", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
