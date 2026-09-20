#!/usr/bin/env python3
"""Send a left-click to the tray helper and check the live window."""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DUMP = ROOT / "tools" / "dump_live_windows.py"

WM_LBUTTONUP = 0x0202
WM_LBUTTONDBLCLK = 0x0203
WM_USER_TRAYICON = 6002


def dump() -> dict:
    raw = subprocess.check_output(["python.exe", str(DUMP)], text=True, encoding="utf-8", errors="replace")
    return json.loads(raw)


def live_windows(payload: dict) -> list[dict]:
    found = []
    for window in payload.get("windows", []):
        title = window.get("title") or ""
        if title in {
            "Herdr-Nachtwächter - Live-Status",
            "Herdr Night Watch - Live Status",
        }:
            found.append(window)
    return found


def click_tray(payload: dict, double: bool) -> int | None:
    import ctypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.PostMessageW.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint,
        ctypes.c_size_t,
        ctypes.c_size_t,
    ]
    user32.PostMessageW.restype = ctypes.c_int
    target = next(
        (
            window
            for window in payload.get("windows", [])
            if window.get("class") == "tray_icon_app"
        ),
        None,
    )
    if target is None:
        return None
    hwnd = int(target["hwnd"])
    # tray-icon delivers clicks as WM_USER_TRAYICON with the mouse
    # message in lParam, not as a raw WM_LBUTTONUP on the helper HWND.
    if double:
        user32.PostMessageW(hwnd, WM_USER_TRAYICON, 0, WM_LBUTTONDBLCLK)
    user32.PostMessageW(hwnd, WM_USER_TRAYICON, 0, WM_LBUTTONUP)
    return hwnd


def main() -> int:
    before = dump()
    if live_windows(before):
        print(json.dumps({"ok": False, "reason": "live_already_open", "before": before}, ensure_ascii=True))
        return 2
    hwnd = click_tray(before, double="--double" in sys.argv)
    if hwnd is None:
        print(json.dumps({"ok": False, "reason": "no_tray_helper", "before": before}, ensure_ascii=True))
        return 3
    deadline = time.time() + 20
    after = before
    while time.time() < deadline:
        time.sleep(0.5)
        after = dump()
        found = live_windows(after)
        if found and any(item.get("visible") and item.get("rect", {}).get("w", 0) >= 80 for item in found):
            print(
                json.dumps(
                    {
                        "ok": True,
                        "clicked_hwnd": hwnd,
                        "live": found,
                        "process_count": len(after.get("processes", [])),
                    },
                    ensure_ascii=True,
                )
            )
            return 0
    print(
        json.dumps(
            {
                "ok": False,
                "reason": "live_not_visible",
                "clicked_hwnd": hwnd,
                "after": after,
            },
            ensure_ascii=True,
        )
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
