#!/usr/bin/env python3
"""Capture Claude Code status-line limits and return a fresh quota snapshot."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
from pathlib import Path
import sys
import tempfile
from typing import Any


MAX_AGE_SECONDS = 24 * 60 * 60
WINDOWS = {"five_hour": "five_hour", "seven_day": "week"}


def state_path() -> Path:
    state_home = os.environ.get("XDG_STATE_HOME")
    base = Path(state_home) if state_home else Path.home() / ".local" / "state"
    return base / "herdr-night-watch" / "claude-quota.json"


def valid_number(value: Any) -> bool:
    return (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(value)
    )


def clean_rate_limits(rate_limits: Any, now: float) -> dict[str, dict[str, int | float]]:
    if not isinstance(rate_limits, dict):
        return {}

    cleaned: dict[str, dict[str, int | float]] = {}
    for source, target in WINDOWS.items():
        window = rate_limits.get(source)
        if not isinstance(window, dict) and target != source:
            window = rate_limits.get(target)
        if not isinstance(window, dict):
            continue
        used = window.get("used_percentage")
        if not valid_number(used) or not 0 <= used <= 100:
            continue

        result: dict[str, int | float] = {"used_percentage": used}
        reset = window.get("resets_at")
        if valid_number(reset):
            if reset <= now:
                continue
            result["resets_at"] = int(reset)
        cleaned[target] = result
    return cleaned


def capture_statusline_payload(payload: Any, now: float | None = None) -> bool:
    if not isinstance(payload, dict) or "rate_limits" not in payload:
        return False
    observed_at = dt.datetime.now(dt.timezone.utc).timestamp() if now is None else now
    windows = clean_rate_limits(payload.get("rate_limits"), observed_at)
    if not windows:
        return False

    destination = state_path()
    destination.parent.mkdir(parents=True, exist_ok=True)
    file_descriptor, temporary_name = tempfile.mkstemp(
        prefix=".claude-quota-", dir=destination.parent
    )
    try:
        os.fchmod(file_descriptor, 0o600)
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as handle:
            json.dump({"captured_at": observed_at, "rate_limits": windows}, handle)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary_name, destination)
    except BaseException:
        try:
            os.close(file_descriptor)
        except OSError:
            pass
        try:
            os.unlink(temporary_name)
        except OSError:
            pass
        raise
    return True


def read_quota(now: float | None = None) -> dict[str, dict[str, int | float]] | None:
    current_time = dt.datetime.now(dt.timezone.utc).timestamp() if now is None else now
    try:
        with state_path().open(encoding="utf-8") as handle:
            cached = json.load(handle)
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None

    if not isinstance(cached, dict):
        return None
    captured_at = cached.get("captured_at")
    if not valid_number(captured_at) or not 0 <= current_time - captured_at <= MAX_AGE_SECONDS:
        return None
    windows = clean_rate_limits(cached.get("rate_limits"), current_time)
    return windows or None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--capture", action="store_true")
    mode.add_argument("--read", action="store_true")
    args = parser.parse_args(argv)

    if args.capture:
        try:
            payload = json.load(sys.stdin)
            capture_statusline_payload(payload)
        except (OSError, UnicodeError, json.JSONDecodeError, TypeError, ValueError):
            pass
        return 0

    json.dump(read_quota(), sys.stdout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
