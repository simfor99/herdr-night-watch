#!/usr/bin/env python3
"""Open the live window, stop only the tray, expect the live window to vanish."""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OPEN = ROOT / "tools" / "verify_tray_open.py"
DUMP = ROOT / "tools" / "dump_live_windows.py"


def dump() -> dict:
    return json.loads(subprocess.check_output(["python.exe", str(DUMP)], text=True))


def live_windows(payload: dict) -> list[dict]:
    return [
        window
        for window in payload.get("windows", [])
        if window.get("title")
        in {"Herdr-Nachtwächter - Live-Status", "Herdr Night Watch - Live Status"}
    ]


def tray_pids(payload: dict) -> list[int]:
    pids = []
    for process in payload.get("processes", []):
        command = process.get("CommandLine") or ""
        if "--live-status" in command:
            continue
        pids.append(int(process["ProcessId"]))
    return pids


def main() -> int:
    opened = subprocess.run(["python.exe", str(OPEN)], check=False, text=True, capture_output=True)
    if opened.returncode != 0:
        print(opened.stdout or opened.stderr)
        return opened.returncode
    before = dump()
    trays = tray_pids(before)
    lives = live_windows(before)
    if not trays or not lives:
        print(json.dumps({"ok": False, "reason": "missing_tray_or_live", "before": before}, ensure_ascii=False))
        return 2
    stop = subprocess.run(
        [
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "Stop-Process -Id " + ",".join(str(pid) for pid in trays) + " -Force",
        ],
        check=False,
        text=True,
        capture_output=True,
    )
    if stop.returncode != 0:
        print(json.dumps({"ok": False, "reason": "stop_failed", "stderr": stop.stderr}))
        return 3
    deadline = time.time() + 4
    after = before
    while time.time() < deadline:
        time.sleep(0.25)
        after = dump()
        if not live_windows(after) and not after.get("processes"):
            print(
                json.dumps(
                    {"ok": True, "stopped_trays": trays, "had_live": lives},
                    ensure_ascii=False,
                )
            )
            return 0
    print(
        json.dumps(
            {
                "ok": False,
                "reason": "live_still_open",
                "stopped_trays": trays,
                "after": after,
            },
            ensure_ascii=False,
        )
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
