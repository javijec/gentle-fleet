#!/usr/bin/env python3
"""Run local Windows and remote native Linux CI on an identical clean commit."""
import argparse
from pathlib import Path
import re
import subprocess
import sys
import uuid
from local_ci import ROOT, clean_head, current_platform

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", required=True, help="Existing SSH config alias or user@host")
    p.add_argument("--remote-root", required=True, help="Absolute Linux checkout path")
    p.add_argument("--profile", choices=("quick", "full"), default="full")
    a = p.parse_args()
    # Restrict arguments used by remote shell/scp to an inert subset.
    if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.@-]*", a.host):
        p.error("Use an SSH config alias or user@host")
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", a.remote_root) or ".." in a.remote_root.split("/"):
        p.error("Use an absolute Linux checkout path without spaces or shell characters")
    if current_platform() != "windows":
        p.error("Run from native Windows")
    sha = clean_head()
    name = uuid.uuid4().hex
    folder = ROOT / "reports" / name
    folder.mkdir(parents=True)
    win, lin = folder / "windows.json", folder / "linux.json"
    remote = a.remote_root.rstrip("/")
    remote_report = f"reports/{name}/linux.json"
    opts = ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=15"]
    local = subprocess.run([sys.executable, str(ROOT / "scripts/ci/local_ci.py"), "--profile", a.profile,
                            "--expect-sha", sha, "--output", str(win)], cwd=ROOT)
    cmd = f"cd {remote} && python3 scripts/ci/local_ci.py --profile {a.profile} --expect-sha {sha} --output {remote_report}"
    remote_run = subprocess.run(["ssh", *opts, a.host, cmd], cwd=ROOT)
    copied = subprocess.run(["scp", *opts, f"{a.host}:{remote}/{remote_report}", str(lin)], cwd=ROOT)
    combined = subprocess.run([sys.executable, str(ROOT / "scripts/ci/report.py"), "--profile", a.profile,
                               "--windows", str(win), "--linux", str(lin),
                               "--output", str(folder / "combined.json")], cwd=ROOT)
    return 1 if any(x.returncode for x in (local, remote_run, copied, combined)) else 0


if __name__ == "__main__":
    sys.exit(main())
