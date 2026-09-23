"""Read Codex rate limits without exposing account or session content."""

import asyncio
import datetime as dt
import json
import os
from pathlib import Path
import shutil
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
        result[label] = {
            "remaining_percent": round(100 - used),
            "reset_epoch": int(reset),
        }
    return result or None


def codex_executable() -> str:
    configured = os.environ.get("CODEX_BIN")
    if configured:
        return configured

    local_bin = Path.home() / ".local" / "bin" / "codex"
    if local_bin.is_file() and os.access(local_bin, os.X_OK):
        return str(local_bin)

    nvm_dir = Path(os.environ.get("NVM_DIR", Path.home() / ".nvm"))
    nvm_codex = [
        candidate
        for candidate in (nvm_dir / "versions" / "node").glob("*/bin/codex")
        if candidate.is_file() and os.access(candidate, os.X_OK)
    ]
    if nvm_codex:
        def version_key(candidate: Path) -> tuple[int, ...]:
            version = candidate.parents[1].name.removeprefix("v")
            try:
                return tuple(int(part) for part in version.split("."))
            except ValueError:
                return ()

        return str(max(nvm_codex, key=version_key))

    path_codex = shutil.which("codex")
    return path_codex or "codex"


async def app_server_quota(now: dt.datetime) -> dict | None:
    codex_bin = codex_executable()
    process_env = os.environ.copy()
    executable_path = Path(codex_bin)
    if executable_path.parent != Path("."):
        executable_dir = str(executable_path.parent)
        process_env["PATH"] = os.pathsep.join(
            path for path in (executable_dir, process_env.get("PATH", "")) if path
        )
    try:
        process = await asyncio.create_subprocess_exec(
            codex_bin, "app-server", "--stdio", stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
            env=process_env,
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
    except (OSError, TimeoutError, asyncio.TimeoutError, BrokenPipeError):
        return None
    finally:
        if process.returncode is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
            try:
                await asyncio.wait_for(process.wait(), timeout=2)
            except (TimeoutError, asyncio.TimeoutError):
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
