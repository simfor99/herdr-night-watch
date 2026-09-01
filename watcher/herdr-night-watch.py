#!/usr/bin/env python3
"""Fail-closed night watcher for current Herdr agent work.

The default night run continuously evaluates every agent currently reported by
Herdr. It starts the quiet period only when none is working. Any uncertainty,
blocked agent, or lost Herdr connection prevents shutdown.
"""

from __future__ import annotations

import argparse
import csv
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import traceback
import uuid
from datetime import datetime, timedelta, timezone
from typing import Any
from urllib.error import URLError
from urllib.request import Request, urlopen


TERMINAL_STATUSES = {"idle", "done"}
REFUSAL_STATUSES = {"blocked", "unknown"}
DEFAULT_POLL_SECONDS = 1
DEFAULT_QUIET_SECONDS = 5
DEFAULT_WARNING_SECONDS = 300
MIN_WARNING_SECONDS = 10
MAX_WARNING_SECONDS = 3600
NETWORK_GRACE_SECONDS = 300
NETWORK_CHECK_INTERVAL_SECONDS = 15
BOOT_MARKER_CACHE_TTL_SECONDS = 30
BOOT_MARKER_RETRY_INTERVAL_SECONDS = 5
BOOT_MARKER_RETRY_ATTEMPTS = 12
BOOT_MARKER_WAIT_DEADLINE_SECONDS = 60
CONFIRM_BOOT_MARKER_DEADLINE_SECONDS = min(DEFAULT_WARNING_SECONDS, 30)
STATUS_BOOT_MARKER_DEADLINE_SECONDS = 15
WINDOWS_COMMAND_TIMEOUT_SECONDS = 30
MIN_ABORT_TIMEOUT_SECONDS = 5
WINDOWS_PATH_SEPARATORS = ("\\", "/")
CANCEL_BOOT_MARKER_DEADLINE_SECONDS = MIN_ABORT_TIMEOUT_SECONDS
CONNECTIVITY_URLS = (
    "https://www.msftconnecttest.com/connecttest.txt",
    "https://www.gstatic.com/generate_204",
)
COMPLETION_HISTORY_LIMIT = 30
DIAGNOSTIC_HISTORY_LIMIT = 500
def windows_path_to_wsl(path: str) -> Path | None:
    """Convert an absolute Windows drive path, rejecting drive-relative input."""
    cleaned = path.strip().strip('"')
    if len(cleaned) < 3 or cleaned[1] != ":" or not cleaned[0].isalpha():
        return None
    if cleaned[2] not in WINDOWS_PATH_SEPARATORS:
        return None
    rest = cleaned[3:].replace("\\", "/").lstrip("/")
    return Path(f"/mnt/{cleaned[0].lower()}/{rest}")


def default_windows_install_log_dir() -> Path:
    env = os.environ.get("HERDR_NIGHT_WATCH_LOG_DIR")
    if env:
        return windows_path_to_wsl(env) or Path(env)
    users = Path("/mnt/c/Users")
    preferred = (os.environ.get("USER") or os.environ.get("USERNAME") or "").casefold()
    discovered: list[Path] = []
    if users.is_dir():
        try:
            children = list(users.iterdir())
        except OSError:
            children = []
        for child in children:
            apps = child / "Apps" / "HerdrNachtwaechter"
            if apps.is_dir():
                discovered.append(apps / "logs")
    discovered.sort()
    for candidate in discovered:
        if preferred and candidate.parts and candidate.parts[-4].casefold() == preferred:
            return candidate
    if len(discovered) == 1:
        return discovered[0]
    if discovered:
        raise RuntimeError(
            "Several HerdrNachtwaechter installations were found. "
            "Set HERDR_NIGHT_WATCH_LOG_DIR to the correct logs directory."
        )
    return Path("/mnt/c/Users/Public/HerdrNachtwaechter/logs")


WINDOWS_INSTALL_LOG_DIR: Path | None = None


def windows_install_log_dir() -> Path:
    """Resolve the Windows log directory only when a history operation needs it."""
    global WINDOWS_INSTALL_LOG_DIR
    if WINDOWS_INSTALL_LOG_DIR is None:
        WINDOWS_INSTALL_LOG_DIR = default_windows_install_log_dir()
    return WINDOWS_INSTALL_LOG_DIR
LIVE_MONITORING_SCOPE = "live_agents"
DEFAULT_COMPLETION_ACTION = "shutdown"
COMPLETION_ACTIONS = {"sleep", "shutdown"}
STATE_SCHEMA_VERSION = 4
_RUNTIME_BOOT_ID: str | None = None
_RUNTIME_BOOT_ID_READ_AT: float | None = None


class ShutdownAbortError(RuntimeError):
    """The Windows shutdown was accepted but could not be cancelled safely."""


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def iso_now() -> str:
    return utc_now().isoformat()


def parse_time(value: str | None) -> datetime | None:
    return datetime.fromisoformat(value) if value else None


def windows_boot_time_from_marker(boot_id: str | None) -> datetime | None:
    """Extract the Windows boot timestamp from a composite runtime marker."""
    if not isinstance(boot_id, str) or "|windows:" not in boot_id:
        return None
    value = boot_id.split("|windows:", 1)[1].strip()
    if value.endswith("Z"):
        value = value[:-1] + "+00:00"
    # .NET's round-trip format may contain seven fractional digits while
    # Python's datetime accepts at most six. Keep any timezone suffix intact.
    prefix, separator, fractional_and_zone = value.partition(".")
    if separator:
        digit_count = 0
        while digit_count < len(fractional_and_zone) and fractional_and_zone[digit_count].isdigit():
            digit_count += 1
        fraction = fractional_and_zone[:digit_count]
        if len(fraction) > 6:
            value = f"{prefix}.{fraction[:6]}{fractional_and_zone[digit_count:]}"
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError:
        return None
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=timezone.utc)


def state_dir() -> Path:
    root = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state"))
    return root / "herdr-night-watch"


def paths() -> tuple[Path, Path, Path, Path]:
    root = state_dir()
    return root, root / "active-run.json", root / "shutdown-warning.json", root / "watch.lock"


def wsl_boot_id() -> str | None:
    """Return the current WSL/Linux boot marker when the kernel exposes it."""
    try:
        value = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
    except (OSError, UnicodeError):
        return None
    return value or None


def windows_boot_id(deadline: float | None = None) -> str | None:
    """Return Windows' last boot time when this watcher runs inside WSL."""
    queries = (
        (
            "cim",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToString('o')",
        ),
        (
            "legacy_wmi",
            "$os=Get-WmiObject Win32_OperatingSystem; "
            "[System.Management.ManagementDateTimeConverter]::ToDateTime($os.LastBootUpTime).ToUniversalTime().ToString('o')",
        ),
    )
    for index, (method, query) in enumerate(queries):
        timeout = float(WINDOWS_COMMAND_TIMEOUT_SECONDS)
        if deadline is not None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            timeout = min(timeout, max(0.1, remaining))
        try:
            result = subprocess.run(
                [
                    "powershell.exe",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    query,
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            record_diagnostic("WINDOWS_BOOT_MARKER_TIMEOUT", method=method)
            continue
        except (OSError, subprocess.SubprocessError):
            continue
        value = result.stdout.strip()
        if result.returncode == 0 and value:
            if index > 0:
                record_diagnostic("WINDOWS_BOOT_MARKER_FALLBACK", method=method)
            return value
    record_diagnostic("WINDOWS_BOOT_MARKER_UNAVAILABLE")
    return None


def runtime_boot_id(force: bool = False, deadline: float | None = None) -> str | None:
    """Return a cached boot marker, or refresh it for safety-critical paths."""
    global _RUNTIME_BOOT_ID, _RUNTIME_BOOT_ID_READ_AT
    wsl_marker = wsl_boot_id()
    if not wsl_marker:
        return None

    now = time.monotonic()
    if (
        not force
        and _RUNTIME_BOOT_ID is not None
        and _RUNTIME_BOOT_ID_READ_AT is not None
        and now - _RUNTIME_BOOT_ID_READ_AT < BOOT_MARKER_CACHE_TTL_SECONDS
    ):
        return _RUNTIME_BOOT_ID

    cache_path = state_dir() / "boot-marker-cache.json"
    if not force:
        try:
            cached = load_json(cache_path)
        except (OSError, ValueError):
            cached = None
        cached_at = cached.get("cached_at") if cached else None
        if (
            cached
            and cached.get("wsl_boot_id") == wsl_marker
            and isinstance(cached.get("runtime_boot_id"), str)
            and isinstance(cached_at, (int, float))
            and 0 <= time.time() - cached_at < BOOT_MARKER_CACHE_TTL_SECONDS
        ):
            _RUNTIME_BOOT_ID = cached["runtime_boot_id"]
            _RUNTIME_BOOT_ID_READ_AT = now
            return _RUNTIME_BOOT_ID

    windows_marker = windows_boot_id(deadline=deadline)
    if not windows_marker:
        return None
    _RUNTIME_BOOT_ID = f"wsl:{wsl_marker}|windows:{windows_marker}"
    _RUNTIME_BOOT_ID_READ_AT = now
    try:
        write_json(
            cache_path,
            {
                "cached_at": time.time(),
                "runtime_boot_id": _RUNTIME_BOOT_ID,
                "wsl_boot_id": wsl_marker,
            },
        )
    except OSError:
        pass
    return _RUNTIME_BOOT_ID


def reset_stale_run_after_restart(
    force: bool = False,
    current_boot_id: str | None = None,
    boot_marker_deadline: float | None = None,
) -> bool:
    """Finish an armed run left behind by a Windows/WSL restart.

    The shutdown process can terminate the watcher before it writes its final
    outcome. A new kernel boot must never inherit that incomplete run and its
    pending warning file.
    """
    _, state_path, warning_path, _ = paths()
    if current_boot_id is None:
        current_boot_id = runtime_boot_id(force=force, deadline=boot_marker_deadline)
    if not current_boot_id:
        return False
    completed_run_id: str | None = None
    with completion_lock():
        state = load_json(state_path)
        if not state:
            return False
        previous_boot_id = state.get("boot_id")
        if state.get("outcome"):
            # Completion history is already persisted separately. Once the
            # machine has booted again, the previous finished run must no
            # longer be exposed as the current night-watch state.
            completed_at = parse_time(state.get("finished_at")) or parse_time(
                state.get("armed_at")
            )
            current_windows_boot = windows_boot_time_from_marker(current_boot_id)
            completed_before_current_boot = bool(
                completed_at
                and current_windows_boot
                and completed_at < current_windows_boot
            )
            if (previous_boot_id and previous_boot_id != current_boot_id) or (
                not previous_boot_id and completed_before_current_boot
            ):
                completed_run_id = str(state.get("run_id", "unknown"))
                state_path.unlink(missing_ok=True)
                warning_path.unlink(missing_ok=True)
            else:
                return False
        if completed_run_id is None:
            reset_reason: str | None = None
            if previous_boot_id and previous_boot_id != current_boot_id:
                reset_reason = "boot_id_changed"
            elif not previous_boot_id:
                # Runs created by watcher schema 3 predate persisted boot markers.
                # Only clear them when their armed time is demonstrably before the
                # current Windows boot. This keeps a normal watcher restart on the
                # same machine from cancelling a still-valid legacy run.
                armed_at = parse_time(state.get("armed_at"))
                current_windows_boot = windows_boot_time_from_marker(current_boot_id)
                if armed_at and current_windows_boot and armed_at < current_windows_boot:
                    reset_reason = "legacy_state_after_reboot"
            if not reset_reason:
                return False

            run_id = str(state.get("run_id", "unknown"))
            state["outcome"] = "reset_after_restart"
            state["finished_at"] = iso_now()
            state["detail"] = "night watch reset after WSL/Windows restart"
            state["reset_reason"] = reset_reason
            write_json(state_path, state)
            warning_path.unlink(missing_ok=True)
            try:
                record_cancellation("system_restart", run_id)
            except (OSError, UnicodeError, csv.Error) as error:
                log(f"CANCELLATION HISTORY ERROR {error}")
            log(
                f"RESET run={run_id} reason={state['reset_reason']} "
                "active night watch was cleared after restart"
            )
    if completed_run_id is not None:
        log(
            f"RESET completed run={completed_run_id} after WSL/Windows restart; "
            "current night watch state cleared"
        )
        return True
    return True


def wait_for_runtime_boot_id(deadline_seconds: float = BOOT_MARKER_WAIT_DEADLINE_SECONDS) -> str:
    """Wait for both boot markers without exceeding one monotonic deadline."""
    deadline = time.monotonic() + max(0.0, float(deadline_seconds))
    for attempt in range(BOOT_MARKER_RETRY_ATTEMPTS):
        if time.monotonic() >= deadline:
            break
        boot_id = runtime_boot_id(force=True, deadline=deadline)
        if boot_id:
            return boot_id
        remaining = deadline - time.monotonic()
        if attempt + 1 < BOOT_MARKER_RETRY_ATTEMPTS and remaining > 0:
            log(
                "Complete WSL/Windows boot marker unavailable; "
                f"waiting {min(BOOT_MARKER_RETRY_INTERVAL_SECONDS, remaining):.1f} seconds before retry"
            )
            time.sleep(min(BOOT_MARKER_RETRY_INTERVAL_SECONDS, remaining))
    raise RuntimeError(
        "Complete WSL/Windows boot marker unavailable; refusing to continue the night watch"
    )


def confirmation_boot_marker_deadline_seconds() -> float:
    """Keep confirmation boot-marker refresh inside the active warning window."""
    _, _, warning_path, _ = paths()
    try:
        warning = load_json(warning_path)
        cancelable_until = parse_time(warning.get("cancelable_until")) if warning else None
    except (OSError, UnicodeError, ValueError):
        cancelable_until = None
    except TypeError:
        cancelable_until = None
    if cancelable_until is None:
        return CONFIRM_BOOT_MARKER_DEADLINE_SECONDS
    remaining = max(0.0, (cancelable_until - utc_now()).total_seconds())
    return min(CONFIRM_BOOT_MARKER_DEADLINE_SECONDS, remaining)


def completion_lock_path() -> Path:
    root, _, _, _ = paths()
    return root / "completion.lock"


@contextmanager
def completion_lock() -> Any:
    """Serialize cancel, confirmation, and the irreversible completion step."""
    path = completion_lock_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def settings_path() -> Path:
    return state_dir() / "settings.json"


def completion_action(value: object) -> str:
    return value if isinstance(value, str) and value in COMPLETION_ACTIONS else DEFAULT_COMPLETION_ACTION


def warning_seconds(value: object) -> int:
    return value if isinstance(value, int) and MIN_WARNING_SECONDS <= value <= MAX_WARNING_SECONDS else DEFAULT_WARNING_SECONDS


def settings() -> dict[str, Any]:
    return load_json(settings_path()) or {}


def preferred_completion_action() -> str:
    return completion_action(settings().get("completion_action"))


def preferred_warning_seconds() -> int:
    return warning_seconds(settings().get("warning_seconds"))


def set_completion_action(action: str) -> int:
    if action not in COMPLETION_ACTIONS:
        raise RuntimeError("Unsupported completion action.")
    reset_stale_run_after_restart(
        force=True,
        boot_marker_deadline=time.monotonic() + STATUS_BOOT_MARKER_DEADLINE_SECONDS,
    )
    _, state_path, _, _ = paths()
    state = load_json(state_path)
    if state and not state.get("outcome"):
        raise RuntimeError("Stop the active night watch before changing its completion action.")
    next_settings = settings()
    next_settings["completion_action"] = action
    write_json(settings_path(), next_settings)
    log(f"SETTINGS completion_action={action}")
    return 0


def set_warning_seconds(seconds: int) -> int:
    if not MIN_WARNING_SECONDS <= seconds <= MAX_WARNING_SECONDS:
        raise RuntimeError(
            f"Warning seconds must be between {MIN_WARNING_SECONDS} and {MAX_WARNING_SECONDS}."
        )
    reset_stale_run_after_restart(
        force=True,
        boot_marker_deadline=time.monotonic() + STATUS_BOOT_MARKER_DEADLINE_SECONDS,
    )
    _, state_path, _, _ = paths()
    state = load_json(state_path)
    if state and not state.get("outcome"):
        raise RuntimeError("Stop the active night watch before changing its warning seconds.")
    next_settings = settings()
    next_settings["warning_seconds"] = seconds
    write_json(settings_path(), next_settings)
    log(f"SETTINGS warning_seconds={seconds}")
    return 0


def log(message: str) -> None:
    root, _, _, _ = paths()
    root.mkdir(parents=True, exist_ok=True)
    line = f"{iso_now()} {message}"
    print(line, flush=True)
    with (root / "watch.log").open("a", encoding="utf-8") as handle:
        handle.write(line + "\n")
    record_diagnostic(message)


def record_diagnostic(message: str, **details: object) -> None:
    """Write a bounded, machine-readable incident trail for later diagnosis."""
    root, _, _, _ = paths()
    path = root / "diagnostics.jsonl"
    event, _, remainder = message.partition(" ")
    entry: dict[str, object] = {
        "timestamp": iso_now(),
        "event": event,
        "message": remainder or message,
        "pid": os.getpid(),
        **details,
    }
    try:
        rows: list[str] = []
        if path.exists():
            rows = path.read_text(encoding="utf-8").splitlines()[-(DIAGNOSTIC_HISTORY_LIMIT - 1):]
        rows.append(json.dumps(entry, ensure_ascii=False, sort_keys=True))
        temporary = path.with_suffix(".tmp")
        temporary.write_text("\n".join(rows) + "\n", encoding="utf-8")
        temporary.replace(path)
    except (OSError, UnicodeError) as error:
        print(f"{iso_now()} DIAGNOSTIC LOG ERROR {error}", file=sys.stderr, flush=True)


def record_completion(action: str, trigger: str, run_id: str) -> None:
    """Keep the latest 30 actual Windows completion requests beside the installed app."""
    timestamp = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    action_label = "Energiesparmodus angefordert" if action == "sleep" else "Herunterfahren angefordert"
    trigger_label = {
        "agents_finished": "Herdr-Agenten fertig",
        "network_unavailable": "Internet seit 5 Minuten nicht erreichbar",
        "confirmed": "Sofortbestätigung",
    }.get(trigger, trigger)
    try:
        path = windows_install_log_dir() / "completion-history.csv"
        rows: list[dict[str, str]] = []
        if path.exists():
            with path.open("r", encoding="utf-8", newline="") as handle:
                rows = list(csv.DictReader(handle, delimiter=";"))
        rows = rows[-(COMPLETION_HISTORY_LIMIT - 1):]
        rows.append(
            {
                "Datum und Uhrzeit": timestamp,
                "Aktion": action_label,
                "Auslöser": trigger_label,
                "Lauf-ID": run_id,
            }
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".tmp")
        with temporary.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.DictWriter(
                handle,
                fieldnames=["Datum und Uhrzeit", "Aktion", "Auslöser", "Lauf-ID"],
                delimiter=";",
            )
            writer.writeheader()
            writer.writerows(rows)
        temporary.replace(path)
        log(f"COMPLETION HISTORY action={action} trigger={trigger} path={path}")
    except (OSError, UnicodeError, ValueError, RuntimeError, csv.Error) as error:
        log(f"COMPLETION HISTORY ERROR {error}")
        raise RuntimeError(f"Completion history could not be persisted: {error}") from error


def record_cancellation(source: str, run_id: str) -> None:
    timestamp = datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
    try:
        path = windows_install_log_dir() / "cancellation-history.csv"
        rows: list[dict[str, str]] = []
        if path.exists():
            with path.open("r", encoding="utf-8", newline="") as handle:
                rows = list(csv.DictReader(handle, delimiter=";"))
        rows = rows[-(COMPLETION_HISTORY_LIMIT - 1):]
        rows.append(
            {
                "Datum und Uhrzeit": timestamp,
                "Auslöser": source,
                "Lauf-ID": run_id,
            }
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".tmp")
        with temporary.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.DictWriter(
                handle,
                fieldnames=["Datum und Uhrzeit", "Auslöser", "Lauf-ID"],
                delimiter=";",
            )
            writer.writeheader()
            writer.writerows(rows)
        temporary.replace(path)
        log(f"CANCELLATION HISTORY source={source} path={path}")
    except (OSError, UnicodeError, ValueError, RuntimeError, csv.Error) as error:
        log(f"CANCELLATION HISTORY ERROR {error}")


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def active_run(run_id: str) -> dict[str, Any] | None:
    """Return the currently armed matching run, never a stale in-memory copy."""
    _, state_path, _, _ = paths()
    state = load_json(state_path)
    if not state or state.get("run_id") != run_id or state.get("outcome"):
        return None
    return state


def load_json(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def run_command(arguments: list[str]) -> str:
    completed = subprocess.run(arguments, text=True, capture_output=True, check=False)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or f"exit code {completed.returncode}"
        raise RuntimeError(f"{' '.join(arguments[:3])} failed: {detail}")
    return completed.stdout


def herdr_executable() -> str:
    """Resolve Herdr without depending on an interactive shell profile.

    Windows Task Scheduler starts WSL with a minimal PATH, which normally does
    not include ~/.local/bin even though that is Herdr's standard install path.
    """
    configured = os.environ.get("HERDR_BIN")
    candidates = [configured, shutil.which("herdr"), str(Path.home() / ".local" / "bin" / "herdr")]
    for candidate in candidates:
        if candidate and Path(candidate).is_file() and os.access(candidate, os.X_OK):
            return candidate
    raise RuntimeError("Herdr CLI is not available in this WSL distribution.")


def herdr_command(*arguments: str) -> list[str]:
    return [herdr_executable(), *arguments]


def run_json(arguments: list[str]) -> dict[str, Any]:
    output = run_command(arguments).strip()
    try:
        return json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"Herdr returned invalid JSON: {error}") from error


def verify_herdr() -> None:
    output = run_command(herdr_command("status", "server"))
    if "status: running" not in output or "compatible: yes" not in output:
        raise RuntimeError("Herdr server is not running or the CLI is incompatible.")


def list_agents() -> list[dict[str, Any]]:
    payload = run_json(herdr_command("agent", "list"))
    agents = payload.get("result", {}).get("agents")
    if not isinstance(agents, list):
        raise RuntimeError("Herdr agent list did not contain agents.")
    return agents


def live_agent_summary() -> dict[str, Any]:
    """Return a display-only Herdr summary without affecting safety decisions."""
    try:
        agents = list_agents()
    except RuntimeError as error:
        return {"available": False, "detail": str(error)}
    statuses = [agent.get("agent_status") for agent in agents]
    return {
        "available": True,
        "total": len(agents),
        "working": sum(status == "working" for status in statuses),
        "idle": sum(status == "idle" for status in statuses),
        "done": sum(status == "done" for status in statuses),
        "blocked": sum(status == "blocked" for status in statuses),
        "unknown": sum(status == "unknown" for status in statuses),
    }


def internet_available() -> bool:
    """Require at least one independent connectivity endpoint to answer."""
    for url in CONNECTIVITY_URLS:
        try:
            request = Request(url, headers={"User-Agent": "Herdr-Nachtwaechter/1.0"})
            with urlopen(request, timeout=4) as response:
                if 200 <= response.status < 500:
                    return True
        except (OSError, URLError):
            continue
    return False


class ConnectivityMonitor:
    def __init__(self) -> None:
        self.last_checked_at = 0.0
        self.available = True

    def refresh(self, state: dict[str, Any]) -> bool:
        if time.monotonic() - self.last_checked_at < NETWORK_CHECK_INTERVAL_SECONDS:
            return self.available
        self.last_checked_at = time.monotonic()
        self.available = internet_available()
        offline_since = parse_time(state.get("network_unavailable_since"))
        if self.available and offline_since:
            state["network_unavailable_since"] = None
            write_json(paths()[1], state)
            log("Internet connection restored")
        elif not self.available and not offline_since:
            state["network_unavailable_since"] = iso_now()
            write_json(paths()[1], state)
            log("Internet connection unavailable; five-minute grace period started")
        return self.available


def network_grace_elapsed(state: dict[str, Any]) -> bool:
    unavailable_since = parse_time(state.get("network_unavailable_since"))
    return bool(
        unavailable_since
        and (utc_now() - unavailable_since).total_seconds() >= NETWORK_GRACE_SECONDS
    )


def process_group_id(pane_id: str) -> int:
    payload = run_json(herdr_command("pane", "process-info", "--pane", pane_id))
    value = payload.get("result", {}).get("process_info", {}).get("foreground_process_group_id")
    if not isinstance(value, int):
        raise RuntimeError(f"Herdr did not provide a foreground process group for {pane_id}.")
    return value


def session_id(agent: dict[str, Any]) -> str | None:
    value = agent.get("agent_session", {}).get("value")
    return value if isinstance(value, str) else None


def snapshot_target(agent: dict[str, Any]) -> dict[str, Any]:
    pane_id = agent["pane_id"]
    return {
        "pane_id": pane_id,
        "terminal_id": agent["terminal_id"],
        "agent": agent["agent"],
        "agent_session_id": session_id(agent),
        "process_group_id": process_group_id(pane_id),
        "cwd": agent.get("cwd"),
    }


def arm(dry_run: bool, poll_seconds: int, quiet_seconds: int, warning_seconds: int) -> int:
    boot_id = wait_for_runtime_boot_id()
    reset_stale_run_after_restart(current_boot_id=boot_id)
    root, state_path, warning_path, _ = paths()
    root.mkdir(parents=True, exist_ok=True)
    with completion_lock():
        existing = load_json(state_path)
        if existing and not existing.get("outcome"):
            raise RuntimeError("A night watch is already armed. Use the Windows cancel shortcut first.")
        if warning_path.exists():
            warning = load_json(warning_path) or {}
            cancelable_until = parse_time(warning.get("cancelable_until"))
            if cancelable_until and utc_now() < cancelable_until:
                raise RuntimeError("A Herdr shutdown warning is already active. Cancel it before arming a new run.")
            warning_path.unlink()

        verify_herdr()
        working = [agent for agent in list_agents() if agent.get("agent_status") == "working"]

        state = {
            "schema_version": STATE_SCHEMA_VERSION,
            "demo": False,
            "run_id": str(uuid.uuid4()),
            "boot_id": boot_id,
            "armed_at": iso_now(),
            "dry_run": dry_run,
            "poll_seconds": poll_seconds,
            "quiet_seconds": quiet_seconds,
            "warning_seconds": warning_seconds,
            "all_terminal_since": None,
            "monitoring_scope": LIVE_MONITORING_SCOPE,
            "armed_working_count": len(working),
            "completion_action": preferred_completion_action(),
            "network_unavailable_since": None,
            "targets": [],
        }
        write_json(state_path, state)
    log(
        f"ARMED run={state['run_id']} scope={LIVE_MONITORING_SCOPE} "
        f"working_at_arm={len(working)} completion_action={state['completion_action']} "
        f"warning_seconds={state['warning_seconds']} dry_run={dry_run}"
    )
    return 0


def demo() -> int:
    """Run the complete visible state sequence without reading Herdr or shutting down Windows."""
    boot_id = wait_for_runtime_boot_id()
    reset_stale_run_after_restart(current_boot_id=boot_id)
    root, state_path, warning_path, _ = paths()
    root.mkdir(parents=True, exist_ok=True)
    with completion_lock():
        existing = load_json(state_path)
        if existing and not existing.get("outcome"):
            raise RuntimeError("A night watch is already armed. Stop it before starting the demo.")
        if warning_path.exists():
            warning_path.unlink()
        state = {
            "schema_version": STATE_SCHEMA_VERSION,
            "demo": True,
            "run_id": str(uuid.uuid4()),
            "boot_id": boot_id,
            "armed_at": iso_now(),
            "dry_run": True,
            "poll_seconds": 1,
            "quiet_seconds": 8,
            "warning_seconds": 15,
            "all_terminal_since": None,
            "monitoring_scope": "demo",
            "completion_action": "shutdown",
            "targets": [],
        }
        write_json(state_path, state)
    log(f"DEMO ARMED run={state['run_id']} - no Windows shutdown will occur")
    return 0


def target_status(target: dict[str, Any], agents_by_pane: dict[str, dict[str, Any]]) -> tuple[str | None, str | None]:
    agent = agents_by_pane.get(target["pane_id"])
    if not agent:
        return None, "selected pane is no longer an Herdr agent"
    if agent.get("terminal_id") != target["terminal_id"] or agent.get("agent") != target["agent"]:
        return None, "selected pane now hosts a different terminal or agent"
    expected_session = target.get("agent_session_id")
    if expected_session and session_id(agent) != expected_session:
        return None, "selected pane now hosts a different agent session"
    if process_group_id(target["pane_id"]) != target["process_group_id"]:
        return None, "selected pane process changed"
    status = agent.get("agent_status")
    if not isinstance(status, str):
        return None, "Herdr did not provide an agent status"
    return status, None


def evaluate_live_agents() -> tuple[str, list[str]]:
    agents = list_agents()
    statuses: list[str] = []
    for index, agent in enumerate(agents, start=1):
        pane_id = agent.get("pane_id")
        label = pane_id if isinstance(pane_id, str) and pane_id else f"agent-{index}"
        status = agent.get("agent_status")
        if not isinstance(status, str):
            return "refuse", [f"{label}=missing-status"]
        statuses.append(f"{label}={status}")
        if status in REFUSAL_STATUSES:
            return "refuse", statuses
        if status not in TERMINAL_STATUSES:
            return "active", statuses
    return "terminal", statuses or ["live_agents=0"]


def evaluate_snapshot(state: dict[str, Any]) -> tuple[str, list[str]]:
    agents_by_pane = {agent.get("pane_id"): agent for agent in list_agents()}
    statuses: list[str] = []
    for target in state["targets"]:
        status, reason = target_status(target, agents_by_pane)
        if reason:
            return "refuse", [f"{target['pane_id']}: {reason}"]
        statuses.append(f"{target['pane_id']}={status}")
        if status in REFUSAL_STATUSES:
            return "refuse", statuses
        if status not in TERMINAL_STATUSES:
            return "active", statuses
    return "terminal", statuses


def evaluate(state: dict[str, Any]) -> tuple[str, list[str]]:
    if state.get("demo"):
        return "terminal", ["demo=all_agents_finished"]
    verify_herdr()
    if state.get("monitoring_scope") == LIVE_MONITORING_SCOPE:
        return evaluate_live_agents()
    return evaluate_snapshot(state)


def schedule_shutdown(state: dict[str, Any], reason: str = "agents_finished") -> None:
    _, _, warning_path, _ = paths()
    warning_seconds = state["warning_seconds"]
    cancelable_until = utc_now() + timedelta(seconds=warning_seconds)
    warning = {
        "run_id": state["run_id"],
        "scheduled_at": iso_now(),
        "cancelable_until": cancelable_until.isoformat(),
        "completion_action": completion_action(state.get("completion_action")),
        "reason": reason,
    }
    write_json(warning_path, warning)
    if state["dry_run"]:
        log(
            "DRY RUN: would schedule Windows "
            f"{warning['completion_action']} in {warning_seconds} seconds"
        )
        return
    if warning["completion_action"] == "sleep":
        log(f"Windows sleep will be requested in {warning_seconds} seconds reason={reason}")
        return
    if not shutil.which("powershell.exe"):
        warning_path.unlink(missing_ok=True)
        raise RuntimeError("powershell.exe is unavailable; refusing shutdown.")
    # Keep the warning entirely inside the watcher. Starting an equally long
    # Windows countdown here races the final history write: Windows reaches
    # its deadline first and can terminate WSL before the CSV is durable.
    # The final branch records the request and only then asks Windows to shut
    # down immediately.
    log(f"Windows shutdown will be requested in {warning_seconds} seconds reason={reason}")


def request_windows_sleep() -> None:
    if not shutil.which("powershell.exe"):
        raise RuntimeError("powershell.exe is unavailable; refusing sleep mode.")
    command = (
        "$ErrorActionPreference = 'Stop'; "
        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; "
        "public static class HerdrPower { [DllImport(\"powrprof.dll\", SetLastError=true)] "
        "public static extern bool SetSuspendState(bool hibernate, bool force, bool disableWakeEvent); }'; "
        "if (-not [HerdrPower]::SetSuspendState($false, $false, $false)) { "
        "throw 'Windows could not enter sleep mode.' }"
    )
    completed = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or f"exit code {completed.returncode}"
        raise RuntimeError(f"Windows could not enter sleep mode: {detail}")
    log("Windows sleep requested")


def request_windows_shutdown_now() -> None:
    if not shutil.which("powershell.exe"):
        raise RuntimeError("powershell.exe is unavailable; refusing immediate shutdown.")
    command = (
        "$ErrorActionPreference = 'Stop'; "
        "& shutdown.exe /s /t 0 /d p:4:1 "
        "/c 'Herdr night watch: completion confirmed.'"
    )
    completed = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or f"exit code {completed.returncode}"
        raise RuntimeError(f"Windows could not shut down immediately: {detail}")
    log("Windows shutdown requested immediately after user confirmation")


def abort_shutdown_if_ours(expected_run_id: str | None = None) -> bool | None:
    """Remove our watcher-owned warning without touching other shutdowns."""
    _, _, warning_path, _ = paths()
    try:
        warning = load_json(warning_path)
    except (OSError, UnicodeError, ValueError) as error:
        log(f"Watcher shutdown warning could not be read: {error}")
        return False
    if not warning:
        return None
    if expected_run_id and warning.get("run_id") != expected_run_id:
        log("Watcher shutdown warning belongs to a different run; leaving it untouched")
        return None
    try:
        warning_path.unlink(missing_ok=True)
    except OSError as error:
        log(f"Watcher shutdown warning could not be removed: {error}")
        return False
    log(
        "Windows sleep warning aborted"
        if completion_action(warning.get("completion_action")) == "sleep"
        else "Watcher shutdown warning aborted"
    )
    return True


def abort_shutdown_or_fail(
    state: dict[str, Any],
    detail: str,
    *,
    require_warning: bool = False,
) -> bool:
    """Abort the active warning or finish the run if cancellation is unsafe."""
    result = abort_shutdown_if_ours(state.get("run_id"))
    if result is True or (result is None and not require_warning):
        return True
    finish(state, "shutdown_abort_failed", detail)
    return False


def confirm_completion() -> int:
    boot_marker_deadline = confirmation_boot_marker_deadline_seconds()
    if boot_marker_deadline <= 0:
        raise RuntimeError("The Herdr completion warning is no longer active.")
    boot_id = wait_for_runtime_boot_id(boot_marker_deadline)
    reset_stale_run_after_restart(force=True, current_boot_id=boot_id)
    _, _, warning_path, _ = paths()
    with completion_lock():
        state = load_json(paths()[1])
        warning = load_json(warning_path)
        if not state or state.get("outcome"):
            raise RuntimeError("No active night watch is available for confirmation.")
        if not warning or warning.get("run_id") != state.get("run_id"):
            raise RuntimeError("No matching Herdr completion warning is active.")
        cancelable_until = parse_time(warning.get("cancelable_until"))
        if not cancelable_until or utc_now() >= cancelable_until:
            raise RuntimeError("The Herdr completion warning is no longer active.")
        if state.get("dry_run"):
            warning_path.unlink(missing_ok=True)
            finish(state, "dry_run_confirmed", "observation confirmation - no Windows action")
            log("DRY RUN: confirmation accepted without a Windows action")
            return 0
        action = completion_action(warning.get("completion_action"))
        outcome = "sleep_confirmed" if action == "sleep" else "shutdown_confirmed"
        if abort_shutdown_if_ours(state.get("run_id")) is not True:
            finish(
                state,
                "shutdown_abort_failed",
                "The active completion warning could not be removed before confirmation.",
            )
            raise ShutdownAbortError(
                "The active completion warning could not be removed before confirmation."
            )
        try:
            # The power action may suspend or terminate Windows immediately.
            # Write the durable request record before invoking that action.
            record_completion(action, "confirmed", state["run_id"])
        except RuntimeError as error:
            finish(state, f"{action}_failed", str(error))
            raise
        if action == "shutdown":
            # Persist the terminal state before an immediate shutdown can
            # terminate WSL. A failed Windows request overwrites it below.
            finish(state, outcome, "completion confirmed by the user")
        try:
            if action == "sleep":
                request_windows_sleep()
            else:
                request_windows_shutdown_now()
        except RuntimeError as error:
            finish(state, f"{action}_failed", str(error))
            raise
        if action == "sleep":
            finish(state, outcome, "completion confirmed by the user")
    return 0


def finish(state: dict[str, Any], outcome: str, detail: str) -> None:
    _, state_path, _, _ = paths()
    state["outcome"] = outcome
    state["finished_at"] = iso_now()
    state["detail"] = detail
    write_json(state_path, state)
    log(f"FINISHED outcome={outcome} detail={detail}")


def watch() -> int:
    boot_id = wait_for_runtime_boot_id()
    reset_stale_run_after_restart(current_boot_id=boot_id)
    _, state_path, _, lock_path = paths()
    state = load_json(state_path)
    if not state:
        raise RuntimeError("No armed night watch found. Start it from the Windows shortcut.")
    if state.get("outcome"):
        log(f"Run already finished with outcome={state['outcome']}")
        return 0

    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("w", encoding="utf-8") as lock:
        lock_deadline = time.monotonic() + 15
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= lock_deadline:
                    raise RuntimeError("Another night-watch process is still running after 15 seconds.")
                log("Another night-watch process is stopping; waiting for its lock")
                time.sleep(0.25)
        log(f"WATCHING run={state['run_id']}")
        last_report = ""
        connectivity = ConnectivityMonitor()
        while True:
            current = load_json(state_path)
            if not current or current.get("run_id") != state["run_id"]:
                log("Watch cancelled or replaced")
                return 0
            state = current
            if state.get("outcome"):
                log(f"Run finished elsewhere with outcome={state['outcome']}")
                return 0
            connectivity.refresh(state)
            network_outage_trigger = network_grace_elapsed(state)
            try:
                result, statuses = evaluate(state)
            except RuntimeError as error:
                state["all_terminal_since"] = None
                write_json(state_path, state)
                log(f"Herdr check failed; retrying without shutdown: {error}")
                time.sleep(state["poll_seconds"])
                continue

            report = ", ".join(statuses)
            if report != last_report:
                log(f"STATUS {report}")
                last_report = report
            if result == "refuse":
                finish(state, "refused", report)
                return 0
            if result == "active" and not network_outage_trigger:
                if state.get("all_terminal_since"):
                    state["all_terminal_since"] = None
                    write_json(state_path, state)
                    log("Quiet period reset because Herdr reports active work again")
                time.sleep(state["poll_seconds"])
                continue

            if result == "active":
                warning_reason = "network_unavailable"
                log("Internet has been unavailable for five minutes; starting completion warning")
            else:
                terminal_since = parse_time(state.get("all_terminal_since"))
                if not terminal_since:
                    state["all_terminal_since"] = iso_now()
                    write_json(state_path, state)
                    log(
                        "No Herdr agents are working; "
                        f"quiet period started ({state['quiet_seconds']} seconds)"
                    )
                    time.sleep(state["poll_seconds"])
                    continue
                if (utc_now() - terminal_since).total_seconds() < state["quiet_seconds"]:
                    time.sleep(state["poll_seconds"])
                    continue

                # Recheck immediately before scheduling, then keep watching during the warning.
                try:
                    result, statuses = evaluate(state)
                except RuntimeError as error:
                    state["all_terminal_since"] = None
                    write_json(state_path, state)
                    log(f"Herdr check failed; retrying without shutdown: {error}")
                    time.sleep(state["poll_seconds"])
                    continue
                if result == "refuse":
                    finish(state, "refused", ", ".join(statuses))
                    return 0
                if result != "terminal":
                    log("Final check did not confirm inactivity; continuing without shutdown")
                    state["all_terminal_since"] = None
                    write_json(state_path, state)
                    time.sleep(state["poll_seconds"])
                    continue
                warning_reason = "agents_finished"

            with completion_lock():
                state = active_run(state["run_id"])
                if not state:
                    log("Run cancelled before completion warning could be scheduled")
                    return 0
                try:
                    schedule_shutdown(state, warning_reason)
                except RuntimeError as error:
                    finish(state, "schedule_failed", str(error))
                    return 1
            deadline = utc_now() + timedelta(seconds=state["warning_seconds"])
            warning_interrupted = False
            while utc_now() < deadline:
                time.sleep(min(5, max(1, state["poll_seconds"])))
                if not active_run(state["run_id"]):
                    with completion_lock():
                        if active_run(state["run_id"]):
                            continue
                        abort_shutdown_if_ours(state.get("run_id"))
                    log("Completion warning cancelled outside the watcher")
                    return 0
                if warning_reason == "network_unavailable":
                    if connectivity.refresh(state):
                        if not abort_shutdown_or_fail(
                            state,
                            "Internet connectivity returned but the Windows shutdown warning could not be cancelled",
                            require_warning=True,
                        ):
                            return 1
                        state["all_terminal_since"] = None
                        write_json(state_path, state)
                        log("Completion warning cancelled because internet connectivity returned")
                        warning_interrupted = True
                        break
                    continue
                try:
                    result, statuses = evaluate(state)
                except RuntimeError as error:
                    if not abort_shutdown_or_fail(
                        state,
                        f"Herdr check failed and the Windows shutdown warning could not be cancelled: {error}",
                        require_warning=True,
                    ):
                        return 1
                    state["all_terminal_since"] = None
                    write_json(state_path, state)
                    log(f"Completion warning cancelled because Herdr check failed: {error}")
                    warning_interrupted = True
                    break
                if result == "refuse":
                    if not abort_shutdown_or_fail(
                        state,
                        "Herdr refused completion and the Windows shutdown warning could not be cancelled",
                        require_warning=True,
                    ):
                        return 1
                    finish(state, "refused", ", ".join(statuses))
                    return 0
                if result == "active":
                    if not abort_shutdown_or_fail(
                        state,
                        "Herdr reported active work and the Windows shutdown warning could not be cancelled",
                        require_warning=True,
                    ):
                        return 1
                    state["all_terminal_since"] = None
                    write_json(state_path, state)
                    log("Shutdown warning cancelled because Herdr reports active work again")
                    warning_interrupted = True
                    break
            if warning_interrupted:
                continue
            with completion_lock():
                run_id = state["run_id"]
                state = active_run(run_id)
                if not state:
                    abort_shutdown_if_ours(run_id)
                    log("Completion action cancelled before execution")
                    return 0
                warning = load_json(paths()[2])
                if not warning or warning.get("run_id") != state["run_id"]:
                    log("Completion action skipped because its warning is no longer active")
                    return 0
                if warning_reason == "network_unavailable":
                    if connectivity.refresh(state):
                        if not abort_shutdown_or_fail(
                            state,
                            "Internet connectivity returned but the Windows shutdown warning could not be cancelled",
                            require_warning=True,
                        ):
                            return 1
                        state["all_terminal_since"] = None
                        write_json(state_path, state)
                        log("Completion warning cancelled because internet connectivity returned")
                        continue
                else:
                    try:
                        result, statuses = evaluate(state)
                    except RuntimeError as error:
                        if not abort_shutdown_or_fail(
                            state,
                            f"Final Herdr check failed and the Windows shutdown warning could not be cancelled: {error}",
                            require_warning=True,
                        ):
                            return 1
                        state["all_terminal_since"] = None
                        write_json(state_path, state)
                        log(f"Completion warning cancelled because the final Herdr check failed: {error}")
                        continue
                    if result == "refuse":
                        if not abort_shutdown_or_fail(
                            state,
                            "Herdr refused completion and the Windows shutdown warning could not be cancelled",
                            require_warning=True,
                        ):
                            return 1
                        finish(state, "refused", ", ".join(statuses))
                        return 0
                    if result != "terminal":
                        if not abort_shutdown_or_fail(
                            state,
                            "Herdr reported active work and the Windows shutdown warning could not be cancelled",
                            require_warning=True,
                        ):
                            return 1
                        state["all_terminal_since"] = None
                        write_json(state_path, state)
                        log("Completion warning cancelled because Herdr reports active work again")
                        continue
                action = completion_action(state.get("completion_action"))
                completion_detail = (
                    "internet was unavailable for five minutes through the warning"
                    if warning_reason == "network_unavailable"
                    else "no Herdr agents remained working through the warning"
                )
                if not abort_shutdown_or_fail(
                    state,
                    "The active completion warning could not be removed before completion",
                    require_warning=True,
                ):
                    return 1
                if action == "sleep" and not state.get("demo") and not state.get("dry_run"):
                    try:
                        # Sleep can suspend the process before any following
                        # Python statement runs, so record it first.
                        record_completion(action, warning_reason, state["run_id"])
                        request_windows_sleep()
                    except RuntimeError as error:
                        finish(state, "sleep_failed", str(error))
                        raise
                    finish(state, "sleep_requested", completion_detail)
                    return 0
                outcome = "demo_complete" if state.get("demo") else "shutdown_scheduled"
                detail = (
                    "demo completed without a Windows shutdown"
                    if state.get("demo")
                    else completion_detail
                )
                if action == "shutdown" and not state.get("demo") and not state.get("dry_run"):
                    try:
                        # Record both the request and terminal state before the
                        # immediate Windows action can terminate WSL.
                        record_completion(action, warning_reason, state["run_id"])
                        finish(state, outcome, detail)
                        request_windows_shutdown_now()
                    except RuntimeError as error:
                        finish(state, "shutdown_failed", str(error))
                        return 1
                    return 0
                finish(state, outcome, detail)
                return 0


def cancel(source: str) -> int:
    reset_stale_run_after_restart(
        force=True,
        boot_marker_deadline=time.monotonic() + CANCEL_BOOT_MARKER_DEADLINE_SECONDS,
    )
    _, state_path, _, _ = paths()
    with completion_lock():
        state = load_json(state_path)
        if state and not state.get("outcome"):
            run_id = str(state.get("run_id"))
            finish(state, "cancelled", f"cancelled by {source}")
            abort_shutdown_if_ours(run_id)
            try:
                record_cancellation(source, run_id)
            except (OSError, UnicodeError, csv.Error) as error:
                log(f"CANCELLATION HISTORY ERROR {error}")
            log(f"CANCELLED run={run_id} source={source}")
        elif not state:
            log("No armed night watch found")
            abort_shutdown_if_ours()
        else:
            log(f"Latest run already finished with outcome={state.get('outcome')}")
    return 0


def status() -> int:
    reset_stale_run_after_restart(
        boot_marker_deadline=time.monotonic() + STATUS_BOOT_MARKER_DEADLINE_SECONDS
    )
    _, state_path, warning_path, _ = paths()
    state = load_json(state_path)
    warning = load_json(warning_path)
    if warning:
        cancelable_until = parse_time(warning.get("cancelable_until"))
        if cancelable_until:
            warning["seconds_remaining"] = max(
                0, int((cancelable_until - utc_now()).total_seconds())
            )
    print(
        json.dumps(
            {
                "state": state,
                "shutdown_warning": warning,
                "preferred_completion_action": preferred_completion_action(),
                "preferred_warning_seconds": preferred_warning_seconds(),
                "live_agents": live_agent_summary(),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Fail-closed Herdr night watcher")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--arm", action="store_true", help="Watch all currently reported Herdr agents")
    mode.add_argument("--demo", action="store_true", help="Safely simulate completion, quiet period, and shutdown warning")
    mode.add_argument("--watch", action="store_true", help="Watch an already armed run")
    mode.add_argument("--cancel", action="store_true", help="Cancel the armed run and its own pending shutdown")
    mode.add_argument("--confirm", action="store_true", help="Immediately perform the active completion action after user confirmation")
    mode.add_argument("--status", action="store_true", help="Show the current run state")
    mode.add_argument("--set-completion-action", choices=sorted(COMPLETION_ACTIONS))
    mode.add_argument("--set-warning-seconds", type=int)
    parser.add_argument("--cancel-source", default="external_or_legacy")
    parser.add_argument("--dry-run", action="store_true", help="Do everything except invoke Windows shutdown")
    parser.add_argument("--poll-seconds", type=int, default=DEFAULT_POLL_SECONDS)
    parser.add_argument("--quiet-seconds", type=int, default=DEFAULT_QUIET_SECONDS)
    parser.add_argument("--warning-seconds", type=int)
    arguments = parser.parse_args()
    if arguments.warning_seconds is not None and not (
        MIN_WARNING_SECONDS <= arguments.warning_seconds <= MAX_WARNING_SECONDS
    ):
        parser.error(
            f"warning seconds must be between {MIN_WARNING_SECONDS} and {MAX_WARNING_SECONDS}"
        )
    selected_warning_seconds = arguments.warning_seconds or preferred_warning_seconds()
    if min(arguments.poll_seconds, arguments.quiet_seconds, selected_warning_seconds) < 1:
        parser.error("poll, quiet, and warning seconds must be positive")
    try:
        if arguments.arm:
            return arm(arguments.dry_run, arguments.poll_seconds, arguments.quiet_seconds, selected_warning_seconds)
        if arguments.demo:
            return demo()
        if arguments.watch:
            return watch()
        if arguments.set_completion_action:
            return set_completion_action(arguments.set_completion_action)
        if arguments.set_warning_seconds is not None:
            return set_warning_seconds(arguments.set_warning_seconds)
        if arguments.confirm:
            return confirm_completion()
        if arguments.cancel:
            return cancel(arguments.cancel_source)
        return status()
    except RuntimeError as error:
        log(f"ERROR {error}")
        return 1
    except Exception as error:
        log(f"FATAL {type(error).__name__}: {error}")
        record_diagnostic(
            "FATAL unhandled watcher exception",
            error_type=type(error).__name__,
            traceback=traceback.format_exc(),
        )
        return 1


if __name__ == "__main__":
    sys.exit(main())
