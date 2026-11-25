#!/usr/bin/env python3
"""Read-only publication checks against a disposable private Git mirror.

Requires Git, Python 3.10+ and Gitleaks 8.19+ with git/dir commands.
No network calls, pushes, history rewrites or visibility changes.
Reports are sensitive even with redaction: keep them outside the repository.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace", check=True,
    )
    return result.stdout


def scan(mode: str, target: Path, config: Path, report: Path) -> str:
    args = [
        "gitleaks", mode, "--config", str(config), "--redact=100",
        "--no-banner", "--exit-code=10", "--report-format=json",
        "--report-path", str(report),
    ]
    if mode == "git":
        args.append("--log-opts=--all")
    result = subprocess.run(
        args + [str(target)], stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, check=False,
    )
    return {0: "PASS", 10: "FINDINGS"}.get(result.returncode, "ERROR")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mirror", type=Path)
    parser.add_argument("--reports", type=Path, required=True)
    args = parser.parse_args()
    repo = args.mirror.resolve()
    reports = args.reports.resolve()
    if shutil.which("git") is None or shutil.which("gitleaks") is None:
        parser.error("Git and Gitleaks must be installed")
    if not repo.is_dir() or git(repo, "rev-parse", "--is-bare-repository").strip() != "true":
        parser.error("Use a disposable bare mirror clone")
    if reports == repo or repo in reports.parents:
        parser.error("Reports must be outside the mirror")
    if reports.exists():
        parser.error("Choose a new, private reports directory")
    reports.mkdir(mode=0o700, parents=True)
    config = reports / "default-rules.toml"
    config.write_text('title = "Public audit"\n[extend]\nuseDefault = true\n', encoding="utf-8")

    refs = git(repo, "for-each-ref", "--format=%(refname)").splitlines()
    heads = [ref for ref in refs if ref.startswith("refs/heads/")]
    metadata = git(repo, "log", "--all", "--format=%H%x09%ae%x09%ce").splitlines()
    non_noreply = set()
    affected = set()
    for row in metadata:
        commit, author, committer = row.split("\t", 2)
        for email in (author, committer):
            if email and not re.fullmatch(r"(?:[^@]+@users\.noreply\.github\.com|noreply@github\.com)", email, re.I):
                non_noreply.add(email.casefold())
                affected.add(commit)
    summary = {
        "refs": len(refs),
        "branches": len(heads),
        "tags": sum(ref.startswith("refs/tags/") for ref in refs),
        "pull_refs": sum(ref.startswith("refs/pull/") for ref in refs),
        "commits": len(metadata),
        "non_noreply_email_count": len(non_noreply),
        "commits_with_non_noreply_email": len(affected),
        "history": scan("git", repo, config, reports / "history.json"),
        "snapshots": [],
        "manual_review": "REQUIRED",
    }
    # Worktrees are created only in temporary directories attached to this
    # disposable mirror. A checkout error is an audit failure, not a skip.
    for index, ref in enumerate(heads, 1):
        status = "ERROR"
        with tempfile.TemporaryDirectory(prefix="fleet-audit-") as root:
            checkout = Path(root) / "snapshot"
            attached = False
            try:
                git(repo, "worktree", "add", "--detach", "--quiet", str(checkout), ref)
                attached = True
                status = scan("dir", checkout, config, reports / f"branch-{index}.json")
            except (OSError, subprocess.CalledProcessError):
                status = "ERROR"
            finally:
                if attached:
                    try:
                        git(repo, "worktree", "remove", "--force", str(checkout))
                    except (OSError, subprocess.CalledProcessError):
                        status = "ERROR"
        summary["snapshots"].append({"ref": ref, "status": status, "report": f"branch-{index}.json"})

    (reports / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    # A clean secret scan is not permission to publish; metadata requires
    # explicit review even when the address is intentionally public.
    statuses = [summary["history"]] + [item["status"] for item in summary["snapshots"]]
    if not heads or "ERROR" in statuses:
        return 2
    if "FINDINGS" in statuses or non_noreply:
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, ValueError):
        print("Audit failed; keep the repository private. Review locally without sharing raw logs.")
        raise SystemExit(2)
