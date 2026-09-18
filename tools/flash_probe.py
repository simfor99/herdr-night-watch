"""Measure black flashes of the live-status window.

Grabs only the live-status window region (no full screenshots), computes
per-frame brightness statistics, and reports flash events with timing.

Usage (from WSL, Windows python):
    python.exe tools/flash_probe.py --seconds 150 [--interval 0.2] [--out reports/flash-probe]

Flash event = frame whose mean brightness collapses below half the run median
(dark frame), plus per-event duration in frame counts.
"""

from __future__ import annotations

import argparse
import csv
import json
import statistics
import time
from pathlib import Path

from PIL import ImageGrab

WINDOW_TITLE_NEEDLE = "Live-Status"


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
        if WINDOW_TITLE_NEEDLE not in buffer.value:
            return True
        if not user32.IsWindowVisible(hwnd):
            return True
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        if rect.right - rect.left <= 0 or rect.bottom - rect.top <= 0:
            return True
        result.append((rect.left, rect.top, rect.right, rect.bottom))
        return True

    user32.EnumWindows(callback, 0)
    if not result:
        raise SystemExit("Kein sichtbares Live-Status-Fenster gefunden")
    return result[0]


def frame_stats(image) -> tuple[float, int, float]:
    gray = image.convert("L")
    histogram = gray.histogram()
    total = sum(histogram) or 1
    mean = sum(i * count for i, count in enumerate(histogram)) / total
    dark = sum(histogram[:40]) / total
    min_level = next(i for i, count in enumerate(histogram) if count)
    return mean, min_level, dark


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=float, default=150.0)
    parser.add_argument("--interval", type=float, default=0.2)
    parser.add_argument("--out", type=Path, default=Path("reports/flash-probe"))
    parser.add_argument(
        "--event-threshold",
        type=float,
        default=None,
        help="Mean-Helligkeit, unter der ein Frame als Event gilt und als PNG gespeichert wird (Default: 85%% des Lauf-Medians).",
    )
    args = parser.parse_args()

    rect = find_live_window_rect()
    print(f"Fensterregion: {rect}")

    args.out.mkdir(parents=True, exist_ok=True)
    rows: list[dict] = []
    started = time.time()
    deadline = started + args.seconds
    print(f"Messe {args.seconds:.0f} s mit {args.interval * 1000:.0f} ms Abstand ...")

    reference_median: list[float] = []
    while time.time() < deadline:
        frame_started = time.perf_counter()
        image = ImageGrab.grab(bbox=rect, all_screens=True)
        mean, min_level, dark = frame_stats(image)
        t_now = time.time()
        rows.append(
            {
                "t_epoch": t_now,
                "t_rel": t_now - started,
                "mean": round(mean, 2),
                "min": min_level,
                "dark_ratio": round(dark, 4),
            }
        )
        reference_median.append(mean)
        if args.event_threshold is not None:
            threshold = args.event_threshold
        elif len(reference_median) > 25:
            threshold = statistics.median(reference_median) * 0.85
        else:
            threshold = 0.0
        if threshold and mean < threshold:
            frame_path = args.out / f"event_{t_now - started:07.1f}s_mean{mean:.0f}.png"
            image.save(frame_path)
        elapsed = time.perf_counter() - frame_started
        time.sleep(max(0.0, args.interval - elapsed))

    csv_path = args.out / "frames.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)

    means = [row["mean"] for row in rows]
    median = statistics.median(means)
    threshold = (
        args.event_threshold
        if args.event_threshold is not None
        else median * 0.85
    )
    dark_frames = [row for row in rows if row["mean"] < threshold]

    events: list[dict] = []
    current: list[dict] | None = None
    for row in rows:
        if row["mean"] < threshold:
            if current is None:
                current = [row]
            else:
                current.append(row)
        elif current is not None:
            events.append(
                {
                    "at_s": round(current[0]["t_rel"], 2),
                    "duration_s": round(current[-1]["t_rel"] - current[0]["t_rel"] + args.interval, 2),
                    "min_mean": min(item["mean"] for item in current),
                }
            )
            current = None
    if current is not None:
        events.append(
            {
                "at_s": round(current[0]["t_rel"], 2),
                "duration_s": round(current[-1]["t_rel"] - current[0]["t_rel"] + args.interval, 2),
                "min_mean": min(item["mean"] for item in current),
            }
        )

    gaps = [
        round(events[i + 1]["at_s"] - events[i]["at_s"], 1)
        for i in range(len(events) - 1)
    ]

    report = {
        "window_rect": rect,
        "measured_seconds": round(time.time() - started, 1),
        "frame_count": len(rows),
        "median_mean": round(median, 2),
        "event_threshold": round(threshold, 2),
        "dark_frame_count": len(dark_frames),
        "flash_events": events,
        "event_gaps_s": gaps,
    }
    report_path = args.out / "report.json"
    report_path.write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
    print(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"CSV: {csv_path}")
    print(f"Report: {report_path}")


if __name__ == "__main__":
    main()
