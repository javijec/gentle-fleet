#!/usr/bin/env python3
"""Combine native reports; never certify different commits or incomplete runs."""
import argparse
import json
from pathlib import Path
import sys
from local_ci import PROFILES, clean_head, overall, validate_report, write_json


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--windows", required=True, type=Path)
    p.add_argument("--linux", required=True, type=Path)
    p.add_argument("--profile", choices=PROFILES, default="full")
    p.add_argument("--output", type=Path, default=Path("reports/combined.json"))
    a = p.parse_args()
    data = {"schema": 1, "source": "local", "profile": a.profile,
            "commit": None, "status": "BLOCKED", "platforms": {}}
    try:
        data["commit"] = clean_head()
        for name, path in (("windows", a.windows), ("linux", a.linux)):
            report = json.loads(path.read_text(encoding="utf-8"))
            status = validate_report(report, name, data["commit"], a.profile)
            data["platforms"][name] = {"status": status, "report": str(path),
                                       "checks": report["checks"]}
        data["status"] = overall(data["platforms"].values())
    except (ValueError, RuntimeError, OSError, KeyError) as exc:
        data["reason"] = str(exc)
    write_json(a.output, data)
    print(data["status"], a.output)
    return 0 if data["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
