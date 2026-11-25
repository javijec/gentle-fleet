#!/usr/bin/env python3
"""Run the automated entry gate for W1-C from native Windows."""
import argparse
import subprocess
import sys
import uuid

from local_ci import ROOT, clean_head, current_platform, timestamp, write_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--remote-root", required=True)
    args = parser.parse_args()
    try:
        if current_platform() != "windows":
            raise RuntimeError("Run from native Windows")
        sha = clean_head()
        folder = ROOT / "reports" / ("w1c-entry-" + uuid.uuid4().hex)
        folder.mkdir(parents=True)
        output = folder / "entry.json"
        data = {
            "commit": sha,
            "started": timestamp(),
            "status": "BLOCKED",
            "full": "BLOCKED",
            "protocol_runs": [],
            "manual_review": "REQUIRED",
        }
        write_json(output, data)
        try:
            code = subprocess.run(
                [sys.executable, str(ROOT / "scripts/ci/remote.py"),
                 "--host", args.host, "--remote-root", args.remote_root,
                 "--profile", "full"],
                cwd=ROOT,
            ).returncode
            data["full"] = "PASS" if code == 0 else "FAIL_OR_BLOCKED"
            write_json(output, data)
            if code != 0:
                return 1
            clean_head(sha)
            for index in range(1, 11):
                log = folder / f"protocol-{index:02d}.log"
                print(f"RUN protocol {index}/10", flush=True)
                with log.open("xb") as stream:
                    result = subprocess.run(
                        ["cargo", "test", "-p", "fleet-protocol", "--locked"],
                        cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT,
                    )
                row = {
                    "run": index,
                    "exit_code": result.returncode,
                    "status": "PASS" if result.returncode == 0 else "FAIL",
                    "log": log.name,
                }
                data["protocol_runs"].append(row)
                write_json(output, data)
                if result.returncode != 0:
                    data["status"] = "FAIL"
                    return 1
                clean_head(sha)
            data["status"] = "AUTOMATED_PASS"
            return 0
        finally:
            data["finished"] = timestamp()
            write_json(output, data)
            print(f'{data["status"]}: {output}', flush=True)
            print("Review logs, identity/ACL/cancellation residuals and P0/P1 before W1-C.")
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"BLOCKED: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
