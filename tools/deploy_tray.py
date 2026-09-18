#!/usr/bin/env python3
"""Replace the installed tray EXE and restart only the tray process."""

from __future__ import annotations

import shutil
import subprocess
import time
from pathlib import Path

SRC = Path("/home/simon/projects/herdr-night-watch/target/x86_64-pc-windows-gnu/release/herdr-night-watch.exe")
DST = Path("/mnt/c/Users/Simon/Apps/HerdrNachtwaechter/Herdr-Nachtwaechter.exe")
PREV = Path("/mnt/c/Users/Simon/Apps/HerdrNachtwaechter/Herdr-Nachtwaechter.prev.exe")
WIN_DST = r"C:\Users\Simon\Apps\HerdrNachtwaechter\Herdr-Nachtwaechter.exe"


def ps(command: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", command],
        check=False,
        text=True,
        capture_output=True,
        stdin=subprocess.DEVNULL,
    )


def main() -> int:
    if not SRC.is_file():
        raise SystemExit(f"missing build: {SRC}")
    stop = ps(
        "Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | Stop-Process -Force; "
        "Start-Sleep -Milliseconds 400; "
        "(Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | Measure-Object).Count"
    )
    if stop.returncode != 0:
        raise SystemExit(f"stop failed: {stop.stderr or stop.stdout}")
    leftover = (stop.stdout or "").strip()
    if leftover not in {"", "0"}:
        raise SystemExit(f"tray still running after stop: {leftover}")
    if DST.exists():
        shutil.copy2(DST, PREV)
    shutil.copy2(SRC, DST)
    start = ps(f"Start-Process -FilePath '{WIN_DST}'")
    if start.returncode != 0:
        raise SystemExit(f"start failed: {start.stderr or start.stdout}")
    time.sleep(1.5)
    check = ps(
        "Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | "
        "Select-Object -First 1 -ExpandProperty Id"
    )
    pid = (check.stdout or "").strip()
    if not pid:
        raise SystemExit("tray did not start")
    print(f"deployed pid={pid} bytes={DST.stat().st_size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
