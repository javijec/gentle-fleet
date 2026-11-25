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

from local_ci import ROOT, clean_head, current_platform, run_step, timestamp, validate_report, write_json

SSH_OPTIONS = ["-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
               "-o", "ConnectTimeout=15"]


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
    encoded = base64.b64encode(source.encode("utf-8")).decode("ascii")
    return " -c \"import base64;exec(base64.b64decode('" + encoded + "'))\""


def invoke(host, root, platform, args, sha, capture=False):
    if host == "local":
        cmd = [sys.executable, str(ROOT / "scripts/ci/both.py"), *args]
    else:
        python = "python" if platform == "windows" else "python3"
        cmd = ["ssh", *SSH_OPTIONS, host, python + remote_command(root, args, sha)]
    return subprocess.run(cmd, cwd=ROOT, text=True, encoding="utf-8",
                          errors="replace", stdout=subprocess.PIPE if capture else None)


def worker(args):
    if current_platform() != args.platform:
        raise RuntimeError("Native platform mismatch")
    clean_head(args.sha)
    if not re.fullmatch(r"[0-9a-f]{32}", args.run_id):
        raise ValueError("Invalid run ID")
    folder = ROOT / "reports" / "both" / args.run_id
    if args.worker == "preflight":
        print(f"READY: {args.platform} {args.sha}", flush=True)
        return 0
    if args.worker == "collect":
        print((folder / "native.json").read_text(encoding="utf-8"))
        return 0
    folder.mkdir(parents=True, exist_ok=False)
    profile = "full" if args.mode == "w1c" else args.mode
    report_path = folder / "ci.json"
    record = {"platform": args.platform, "commit": args.sha, "mode": args.mode,
              "started": timestamp(), "status": "BLOCKED", "protocol_runs": []}
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
        if args.mode == "w1c" and args.platform == "windows":
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
        clean_head(args.sha)
        record["status"] = "PASS"
        return 0
    finally:
        record["finished"] = timestamp()
        write_json(native, record)
        print(f'{record["status"]}: {native}', flush=True)


def validate_native(record, platform, sha, mode):
    if (record.get("platform"), record.get("commit"), record.get("mode")) != (platform, sha, mode):
        raise ValueError("Native report identity mismatch")
    if record.get("status") not in ("PASS", "FAIL", "BLOCKED") or not record.get("finished"):
        raise ValueError("Incomplete native report")
    if record["status"] != "PASS":
        return record["status"]
    profile = "full" if mode == "w1c" else mode
    if record.get("ci_exit_code") != 0 or validate_report(record["ci"], platform, sha, profile) != "PASS":
        raise ValueError("Native CI did not pass")
    runs = record["protocol_runs"]
    expected = [f"protocol-{i:02d}" for i in range(1, 11)] if mode == "w1c" and platform == "windows" else []
    if [r["id"] for r in runs] != expected or any(
        r.get("status") != "PASS" or r.get("exit_code") != 0 for r in runs
    ):
        raise ValueError("Missing or failed protocol repetitions")
    return "PASS"


def ask(label, value, default=None):
    if value is not None:
        return value.strip()
    answer = input(label + (f" [{default}]" if default else "") + ": ").strip()
    return answer or default or ""


def coordinator(args):
    sha = clean_head()
    local = current_platform()
    mode = ask("Pruebas (quick/full/w1c)", args.mode, "w1c")
    if mode not in ("quick", "full", "w1c"):
        raise ValueError("Use quick, full or w1c")
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
    run_id = uuid.uuid4().hex
    # The local worker owns the run folder on this machine if selected.
    # Keep coordinator reports in a separate directory.
    summary = ROOT / "reports" / "combined" / run_id
    summary.mkdir(parents=True, exist_ok=False)
    data = {"commit": sha, "mode": mode, "started": timestamp(),
            "status": "BLOCKED", "platforms": {}, "manual_review": "REQUIRED"}
    output = summary / "combined.json"
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
                write_json(summary / f"{platform}.json", record)
                row.update(status=status, report=f"{platform}.json", exit_code=result.returncode)
                if result.returncode != 0 and status == "PASS":
                    row.update(status="BLOCKED", reason="Worker exit code disagrees with report")
            except (OSError, ValueError, RuntimeError, KeyError, subprocess.CalledProcessError) as exc:
                row.update(status="BLOCKED", reason=str(exc))
            finally:
                write_json(output, data)
        clean_head(sha)
        states = {r["status"] for r in data["platforms"].values()}
        if len(data["platforms"]) == 2 and states == {"PASS"}:
            data["status"] = "AUTOMATED_PASS" if mode == "w1c" else "PASS"
        elif "FAIL" in states:
            data["status"] = "FAIL"
        return 0 if data["status"] in ("PASS", "AUTOMATED_PASS") else 1
    finally:
        data["finished"] = timestamp()
        write_json(output, data)
        for platform, row in data["platforms"].items():
            print(f'{platform}: {row["status"]}', flush=True)
        print(f'{data["status"]}: {output}', flush=True)
        print("Review native logs; no manual or release gate is certified.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("quick", "full", "w1c"))
    for platform in ("windows", "linux"):
        parser.add_argument("--" + platform, help="SSH alias or local")
        parser.add_argument("--" + platform + "-root", help="Absolute remote checkout path")
    parser.add_argument("--worker", choices=("preflight", "run", "collect"), help=argparse.SUPPRESS)
    parser.add_argument("--platform", choices=("windows", "linux"), help=argparse.SUPPRESS)
    parser.add_argument("--sha", help=argparse.SUPPRESS)
    parser.add_argument("--run-id", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.worker:
            if not all((args.platform, args.sha, args.run_id, args.mode)):
                parser.error("Incomplete worker arguments")
            return worker(args)
        return coordinator(args)
    except (OSError, RuntimeError, ValueError, KeyError, EOFError, subprocess.CalledProcessError) as exc:
        print(f"BLOCKED: {exc}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("BLOCKED: interrupted", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
