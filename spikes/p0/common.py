"""Shared helpers for the P0 engine probes.

The probes talk to the real CLIs and write anonymised event captures. Nothing in
this module stores credentials: callers pass only the wire data they want kept.
"""

from __future__ import annotations

import json
import os
import platform
import re
from pathlib import Path
from typing import Any, Iterable

HOME = os.path.expanduser("~")
HOSTNAME = platform.node()
MAX_STRING = 1200
UUID_RE = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
# Provider ids keep their shape but lose their value in captures.
ID_PATTERNS = [
    re.compile(r"\bmsg_[A-Za-z0-9]+"),
    re.compile(r"\b(?:ses|per|sh|exec|call|evt)_[A-Za-z0-9]+"),
    re.compile(r"\b0[0-9a-f]{7}-[0-9a-f-]{20,}\b"),
]


def _anonymise_str(value: str) -> str:
    value = value.replace(HOME, "<HOME>")
    if HOSTNAME:
        value = value.replace(HOSTNAME, "<host>")
    value = UUID_RE.sub("<uuid>", value)
    for pattern in ID_PATTERNS:
        value = pattern.sub("<id>", value)
    if len(value) > MAX_STRING:
        value = value[:MAX_STRING] + f"...<truncated {len(value)} chars>"
    return value


def anonymise(value: Any) -> Any:
    if isinstance(value, str):
        return _anonymise_str(value)
    if isinstance(value, list):
        return [anonymise(item) for item in value]
    if isinstance(value, dict):
        return {key: anonymise(item) for key, item in value.items()}
    return value


class Capture:
    """Append newline-delimited JSON events, anonymised on write."""

    def __init__(self, path: str | None):
        self.path = Path(path) if path else None
        if self.path:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            self.path.write_text("")

    def add(self, engine: str, kind: str, payload: Any) -> None:
        if not self.path:
            return
        record = {"engine": engine, "kind": kind, "payload": anonymise(payload)}
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")


def fail(message: str) -> "NoReturn":  # type: ignore[valid-type]
    raise SystemExit(f"FAIL: {message}")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def workdir(prefix: str) -> Path:
    import tempfile

    root = Path(tempfile.mkdtemp(prefix=prefix))
    (root / "cwd").mkdir()
    return root


def binary(env_name: str, default: str) -> str:
    return os.environ.get(env_name, default)
