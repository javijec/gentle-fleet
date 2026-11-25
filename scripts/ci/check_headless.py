#!/usr/bin/env python3
"""Reject desktop/GPUI packages in the locked engine dependency graph."""
import json
import subprocess
import sys

data = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1"],
    text=True,
))
for package in data["packages"]:
    name = package["name"].lower()
    if name == "gentle-fleet-desktop" or name == "gpui" or name.startswith("gpui-"):
        sys.exit("FAIL: graphical dependency in engine: " + name)
print("PASS: headless Cargo metadata")
