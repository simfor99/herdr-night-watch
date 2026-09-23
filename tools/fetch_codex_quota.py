"""Read Codex rate limits without exposing account or session content."""

import asyncio
import datetime as dt
import json
import os
from pathlib import Path
import sys


def normalize_windows(limits: dict, now: dt.datetime, *, app_server: bool = False) -> dict | None:
    result = {}
    for key in ("primary", "secondary"):
        window = limits.get(key)
        if not isinstance(window, dict):
            continue
        used = window.get("usedPercent" if app_server else "used_percent")
        reset = window.get("resetsAt" if app_server else "resets_at")
        duration = window.get("windowDurationMins" if app_server else "window_minutes")
        label = {300: "five_hour", 10080: "week"}.get(duration)
        if label is None or not isinstance(used, (int, float)) or isinstance(used, bool) or not 0 <= used <= 100:
            continue
        if not isinstance(reset, (int, float)) or reset <= now.timestamp():
            continue
        reset_local = dt.datetime.fromtimestamp(reset).astimezone()
        result[label] = {
            "remaining_percent": round(100 - used),
            "reset": reset_local.strftime("%H:%M" if label == "five_hour" else "%d.%m. (%H:%M)"),
        }
    return result or None


async def app_server_quota(now: dt.datetime) -> dict | None:
    try:
        process = await asyncio.create_subprocess_exec(
            "codex", "app-server", "--stdio", stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
        )
    except OSError:
        return None
    try:
        async def request(item: dict, request_id: int) -> dict | None:
            process.stdin.write((json.dumps(item) + "\n").encode())
            await process.stdin.drain()
            while True:
                line = await asyncio.wait_for(process.stdout.readline(), timeout=5)
                if not line:
                    return None
                try:
                    response = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if response.get("id") == request_id:
                    return response.get("result")

        initialized = await request(
            {"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "herdr-night-watch", "version": "0.1"}}}, 1)
        if initialized is None:
            return None
        process.stdin.write(b'{"method":"initialized"}\n')
        await process.stdin.drain()
        response = await request({"id": 2, "method": "account/rateLimits/read",
                                  "params": {"excludeResetCreditDetails": True}}, 2)
        limits = response.get("rateLimits") if isinstance(response, dict) else None
        if isinstance(limits, dict) and limits.get("limitId") == "codex":
            return normalize_windows(limits, now, app_server=True)
        return None
    except (OSError, TimeoutError, BrokenPipeError):
        return None
    finally:
        if process.returncode is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
            try:
                await asyncio.wait_for(process.wait(), timeout=2)
            except TimeoutError:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                await process.wait()


def latest_quota(sessions: Path, now: dt.datetime) -> dict | None:
    today = now.date()
    candidates = []
    for day in (today, today - dt.timedelta(days=1)):
        folder = sessions / f"{day:%Y/%m/%d}"
        if folder.is_dir():
            candidates.extend(folder.glob("rollout-*.jsonl"))
    def modified_time(path: Path) -> float:
        try:
            return path.stat().st_mtime
        except OSError:
            return float("-inf")

    candidates.sort(key=modified_time, reverse=True)

    newest = None
    for path in candidates[:100]:
        try:
            with path.open(encoding="utf-8") as handle:
                for line in handle:
                    try:
                        event = json.loads(line)
                    except json.JSONDecodeError:
                        continue  # A live session may have an incomplete final line.
                    if event.get("payload", {}).get("type") != "token_count":
                        continue
                    limits = event["payload"].get("rate_limits")
                    if (not isinstance(limits, dict) or limits.get("limit_id") != "codex"
                            or not isinstance(limits.get("primary"), dict)):
                        continue
                    try:
                        observed = dt.datetime.fromisoformat(event["timestamp"].replace("Z", "+00:00"))
                    except (KeyError, ValueError, TypeError):
                        continue
                    if observed > now or now - observed > dt.timedelta(hours=24):
                        continue
                    if newest is None or observed > newest[0]:
                        newest = (observed, limits)
        except (OSError, UnicodeError):
            continue

    if newest is None:
        return None

    return normalize_windows(newest[1], now)


def fetch_quota(sessions: Path, now: dt.datetime) -> dict | None:
    try:
        live = asyncio.run(app_server_quota(now))
    except Exception:
        live = None
    return live or latest_quota(sessions, now)


if __name__ == "__main__":
    root = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
    now = dt.datetime.now(dt.timezone.utc)
    json.dump(fetch_quota(root / "sessions", now), sys.stdout)
