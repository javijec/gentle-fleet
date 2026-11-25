"""Compact CI terminal output. JSON reports and log contents remain unchanged."""
import os
import sys

COLORS = {"PASS": "32", "AUTOMATED_PASS": "32", "FAIL": "31",
          "BLOCKED": "33", "SKIP": "33", "RUN": "36", "READY": "36"}


def color_enabled(stream=None):
    stream = sys.stdout if stream is None else stream
    if "NO_COLOR" in os.environ:
        return False
    if os.environ.get("FLEET_CI_COLOR") == "1":
        return True
    if not stream.isatty() or os.environ.get("TERM") == "dumb":
        return False
    if os.name == "nt":
        # Enable ANSI on native Windows consoles; fall back to plain text.
        try:
            import ctypes
            import msvcrt
            from ctypes import wintypes
            kernel = ctypes.windll.kernel32
            kernel.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
            kernel.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
            handle = wintypes.HANDLE(msvcrt.get_osfhandle(stream.fileno()))
            mode = wintypes.DWORD()
            return bool(kernel.GetConsoleMode(handle, ctypes.byref(mode))
                        and kernel.SetConsoleMode(handle, mode.value | 4))
        except (AttributeError, OSError, ValueError):
            return False
    return True


def status_text(status, enabled=None):
    if enabled is None:
        enabled = color_enabled()
    code = COLORS.get(status)
    return f"\x1b[{code}m{status}\x1b[0m" if enabled and code else status


def progress(name):
    if color_enabled():
        print(f"  {status_text('RUN'):s}  {name}", end="", flush=True)
    else:
        print(f"  RUN  {name}", flush=True)


def result_line(row, enabled=None):
    status = row["status"]
    seconds = f" ({row['seconds']:.1f}s)" if "seconds" in row else ""
    return f"  {status_text(status, enabled)}  {row['id']}{seconds}"


def show_result(row, running=False):
    enabled = color_enabled()
    prefix = "\r\x1b[2K" if enabled and running else ""
    print(prefix + result_line(row, enabled), flush=True)
    if row["status"] != "PASS":
        if row.get("reason"):
            print(f"        Reason: {row['reason']}", flush=True)
        if row.get("log"):
            print(f"        Log: {row['log']}", flush=True)


def summary(label, status, rows=(), report=None):
    rows = list(rows)
    counts = {key: sum(row["status"] == key for row in rows)
              for key in ("PASS", "FAIL", "BLOCKED", "SKIP")}
    suffix = " | " + " | ".join(f"{status_text(key)} {value}"
                                  for key, value in counts.items()) if rows else ""
    print(f"{label}: {status_text(status)}{suffix}", flush=True)
    if status not in ("PASS", "AUTOMATED_PASS") and report is not None:
        print(f"  Report: {report}", flush=True)
