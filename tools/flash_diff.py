"""Pixel-Differenz zwischen Event-Frame und Normalframe des Live-Fensters.

Usage:
    python.exe tools/flash_diff.py --event reports/flash-probe-run2/event_*.png
"""

from __future__ import annotations

import argparse
import glob
from pathlib import Path

from PIL import Image, ImageChops, ImageGrab


def find_live_window_rect() -> tuple[int, int, int, int]:
    import ctypes
    import ctypes.wintypes as wt

    user32 = ctypes.windll.user32
    result: list[tuple[int, int, int, int]] = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
    def callback(hwnd, _lparam):
        length = user32.GetWindowTextLengthW(hwnd)
        if length <= 0:
            return True
        buffer = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, buffer, length + 1)
        if "Live-Status" not in buffer.value or not user32.IsWindowVisible(hwnd):
            return True
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        if rect.right - rect.left > 0:
            result.append((rect.left, rect.top, rect.right, rect.bottom))
        return True

    user32.EnumWindows(callback, 0)
    if not result:
        raise SystemExit("Kein sichtbares Live-Status-Fenster gefunden")
    return result[0]


def row_profile(diff) -> list[float]:
    gray = diff.convert("L")
    width, height = gray.size
    data = list(gray.getdata())
    return [
        sum(data[y * width : (y + 1) * width]) / width for y in range(height)
    ]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True)
    args = parser.parse_args()

    matches = glob.glob(args.event)
    if not matches:
        raise SystemExit(f"Kein Match fuer {args.event}")
    event_path = Path(matches[0])

    event = Image.open(event_path).convert("RGB")
    rect = find_live_window_rect()
    reference = ImageGrab.grab(bbox=rect, all_screens=True).convert("RGB")
    if reference.size != event.size:
        print(f"Groessen unterschiedlich: reference={reference.size} event={event.size}")
        return

    reference.save(event_path.parent / "reference_now.png")

    diff = ImageChops.difference(reference, event)
    diff_path = event_path.parent / "diff.png"
    diff.save(diff_path)

    gray = diff.convert("L")
    changed = gray.point(lambda v: 255 if v > 25 else 0)
    bbox = changed.getbbox()
    profile = row_profile(diff)
    height = len(profile)
    band_count = 6
    bands = [
        round(sum(profile[int(i * height / band_count) : int((i + 1) * height / band_count)]) / (height / band_count))
        for i in range(band_count)
    ]
    stats = gray.histogram()
    total = sum(stats) or 1
    print(f"Event: {event_path.name}")
    print(f"Differenz-Bounding-Box (x1,y1,x2,y2): {bbox} von {gray.size}")
    print(f"Bild hoehe in {band_count} Baender, mittlere Diff pro Band (oben->unten): {bands}")
    print(f"Anteil Pixel mit Diff>25: {sum(stats[26:]) / total:.3f}")
    print(f"Anteil Pixel mit Diff>60: {sum(stats[61:]) / total:.3f}")


if __name__ == "__main__":
    main()
