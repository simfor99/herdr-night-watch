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
import urllib.error
import urllib.request


MAX_AGE_SECONDS = 24 * 60 * 60
WINDOWS = {"five_hour": "five_hour", "seven_day": "week"}
# OAuth usage is the primary source: a drained weekly limit stops the very
# Claude sessions whose status lines would refresh the capture, so a pure
# status-line cache freezes at the last pre-drain value (observed 2026-11-09:
# the card kept showing 2% left for hours while the official endpoint already
# reported 100% utilization). Status-line captures remain the 24h fallback for
# environments without stored Claude Code credentials.
OAUTH_MAX_AGE_SECONDS = 10 * 60
OAUTH_WINDOWS = {"five_hour": "five_hour", "seven_day": "week"}
USAGE_URL = "https://api.anthropic.com/api/oauth/usage"


def state_dir() -> Path:
    state_home = os.environ.get("XDG_STATE_HOME")
    base = Path(state_home) if state_home else Path.home() / ".local" / "state"
    return base / "herdr-night-watch"


def state_path() -> Path:
    return state_dir() / "claude-quota.json"


def oauth_state_path() -> Path:
    return state_dir() / "claude-quota-oauth.json"


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


def _store_windows(
    destination: Path, windows: dict[str, dict[str, int | float]], observed_at: float
) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    file_descriptor, temporary_name = tempfile.mkstemp(
        prefix=".claude-quota-", dir=destination.parent
    )
    record: dict[str, Any] = {"captured_at": observed_at, "rate_limits": windows}
    try:
        os.fchmod(file_descriptor, 0o600)
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as handle:
            json.dump(record, handle)
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


def _read_cached_windows(
    path: Path, now: float, max_age: float
) -> dict[str, dict[str, int | float]] | None:
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None
    if not isinstance(cached, dict):
        return None
    captured_at = cached.get("captured_at")
    if not valid_number(captured_at) or not 0 <= now - captured_at <= max_age:
        return None
    return clean_rate_limits(cached.get("rate_limits"), now) or None


def capture_statusline_payload(payload: Any, now: float | None = None) -> bool:
    if not isinstance(payload, dict) or "rate_limits" not in payload:
        return False
    observed_at = dt.datetime.now(dt.timezone.utc).timestamp() if now is None else now
    windows = clean_rate_limits(payload.get("rate_limits"), observed_at)
    if not windows:
        return False
    _store_windows(state_path(), windows, observed_at)
    return True


def oauth_credentials_path() -> Path:
    return Path.home() / ".claude" / ".credentials.json"


def oauth_access_token(now: float) -> str | None:
    """Return the locally stored Claude Code OAuth token while it is valid."""
    try:
        credentials = json.loads(oauth_credentials_path().read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None
    if not isinstance(credentials, dict):
        return None
    oauth = credentials.get("claudeAiOauth")
    if not isinstance(oauth, dict):
        return None
    token = oauth.get("accessToken")
    if not isinstance(token, str) or not token.strip():
        return None
    expires_at = oauth.get("expiresAt")
    if valid_number(expires_at) and expires_at / 1000.0 <= now:
        return None
    return token


def fetch_oauth_windows(now: float) -> dict[str, dict[str, int | float]] | None:
    token = oauth_access_token(now)
    if token is None:
        return None
    request = urllib.request.Request(
        USAGE_URL,
        headers={
            "Authorization": f"Bearer {token}",
            "anthropic-beta": "oauth-2025-04-20",
            "User-Agent": "herdr-night-watch/1.0",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            payload = json.loads(response.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError, UnicodeError):
        return None
    if not isinstance(payload, dict):
        return None

    windows: dict[str, dict[str, int | float]] = {}
    for source, target in OAUTH_WINDOWS.items():
        window = payload.get(source)
        if not isinstance(window, dict):
            continue
        utilization = window.get("utilization")
        if not valid_number(utilization) or not 0.0 <= utilization <= 100.0:
            continue
        entry: dict[str, int | float] = {"used_percentage": utilization}
        resets_at = window.get("resets_at")
        if resets_at is not None:
            epoch: float | None = None
            if isinstance(resets_at, str):
                try:
                    epoch = dt.datetime.fromisoformat(resets_at.replace("Z", "+00:00")).timestamp()
                except ValueError:
                    epoch = None
            elif valid_number(resets_at):
                epoch = resets_at
            # A reset timestamp in the past describes an already-closed
            # window; reporting its old consumption as current would lie.
            if not valid_number(epoch) or epoch <= now:
                continue
            entry["resets_at"] = int(epoch)
        windows[target] = entry
    return windows or None


def read_quota(now: float | None = None) -> dict[str, dict[str, int | float]] | None:
    """Return the freshest trustworthy windows.

    The official usage endpoint answers first (rate-limited by its own
    10-minute cache); a fresh-enough status-line capture is only consulted
    when OAuth cannot deliver (no stored credentials, network down).
    """
    current_time = dt.datetime.now(dt.timezone.utc).timestamp() if now is None else now

    cached_oauth = _read_cached_windows(
        oauth_state_path(), current_time, OAUTH_MAX_AGE_SECONDS
    )
    if cached_oauth:
        return cached_oauth

    windows = fetch_oauth_windows(current_time)
    if windows is not None:
        try:
            _store_windows(oauth_state_path(), windows, current_time)
        except OSError:
            pass
        return windows

    return _read_cached_windows(state_path(), current_time, MAX_AGE_SECONDS)


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
