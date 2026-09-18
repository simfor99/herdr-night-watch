#!/usr/bin/env python3
"""Close the visible Live-Status window if it is open."""

from __future__ import annotations

import ctypes
import json
import subprocess
import time
from pathlib import Path

DUMP = Path("/home/simon/projects/herdr-night-watch/tools/dump_live_windows.py")
WM_CLOSE = 0x0010


def dump() -> dict:
    return json.loads(subprocess.check_output(["python.exe", str(DUMP)], text=True))


def main() -> int:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.PostMessageW.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint,
        ctypes.c_size_t,
        ctypes.c_size_t,
    ]
    user32.PostMessageW.restype = ctypes.c_int
    payload = dump()
    closed = 0
    for window in payload.get("windows", []):
        if window.get("title") in {
            "Herdr-Nachtwächter - Live-Status",
            "Herdr Night Watch - Live Status",
        }:
            user32.PostMessageW(int(window["hwnd"]), WM_CLOSE, 0, 0)
            closed += 1
    deadline = time.time() + 8
    while time.time() < deadline:
        time.sleep(0.3)
        remaining = [
            window
            for window in dump().get("windows", [])
            if window.get("title")
            in {
                "Herdr-Nachtwächter - Live-Status",
                "Herdr Night Watch - Live Status",
            }
        ]
        if not remaining:
            print(json.dumps({"ok": True, "closed": closed}))
            return 0
    print(json.dumps({"ok": False, "closed": closed}))
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
