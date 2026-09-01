#!/usr/bin/env python3
"""Focused safety tests for the Herdr night watcher's decision function."""

from __future__ import annotations

import importlib.util
import csv
import json
import tempfile
from contextlib import ExitStack, nullcontext
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from datetime import datetime, timedelta, timezone


SCRIPT = Path(__file__).with_name("herdr-night-watch.py")
SPEC = importlib.util.spec_from_file_location("herdr_night_watch", SCRIPT)
assert SPEC and SPEC.loader
WATCHER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WATCHER)


def agent(pane_id: str, status: str | None) -> dict[str, object]:
    return {"pane_id": pane_id, "agent_status": status}


LIVE_STATE = {"demo": False, "monitoring_scope": "live_agents", "targets": []}


class LiveScopeEvaluationTests(unittest.TestCase):
    def evaluate(self, agents: list[dict[str, object]]) -> tuple[str, list[str]]:
        with (
            patch.object(WATCHER, "verify_herdr"),
            patch.object(WATCHER, "list_agents", return_value=agents),
        ):
            return WATCHER.evaluate(LIVE_STATE)

    def test_no_working_agents_is_terminal(self) -> None:
        result, statuses = self.evaluate([agent("p1", "idle"), agent("p2", "done")])
        self.assertEqual(result, "terminal")
        self.assertEqual(statuses, ["p1=idle", "p2=done"])

    def test_new_or_existing_work_keeps_the_run_active(self) -> None:
        result, statuses = self.evaluate([agent("p1", "idle"), agent("p-new", "working")])
        self.assertEqual(result, "active")
        self.assertEqual(statuses, ["p1=idle", "p-new=working"])

    def test_unknown_or_missing_status_refuses_shutdown(self) -> None:
        result, _ = self.evaluate([agent("p1", "unknown")])
        self.assertEqual(result, "refuse")

        result, _ = self.evaluate([agent("p1", None)])
        self.assertEqual(result, "refuse")


class CompletionActionTests(unittest.TestCase):
    def test_unknown_settings_fail_closed_to_shutdown(self) -> None:
        self.assertEqual(WATCHER.completion_action("sleep"), "sleep")
        self.assertEqual(WATCHER.completion_action("shutdown"), "shutdown")
        self.assertEqual(WATCHER.completion_action("something else"), "shutdown")
        self.assertEqual(WATCHER.completion_action(["sleep"]), "shutdown")
        self.assertEqual(WATCHER.warning_seconds(10), 10)
        self.assertEqual(WATCHER.warning_seconds(9), 300)

    def test_sleep_warning_never_starts_a_windows_shutdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, warning_path, root / "watch.log")),
                patch.object(WATCHER, "shutil") as shutil_mock,
                patch.object(WATCHER, "subprocess") as subprocess_mock,
            ):
                WATCHER.schedule_shutdown(
                    {
                        "run_id": "test-run",
                        "warning_seconds": 300,
                        "completion_action": "sleep",
                        "dry_run": False,
                    }
                )
            self.assertEqual(WATCHER.load_json(warning_path)["completion_action"], "sleep")
            shutil_mock.which.assert_not_called()
            subprocess_mock.run.assert_not_called()

    def test_warning_setting_preserves_the_completion_action(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock")),
                patch.object(WATCHER, "settings_path", return_value=root / "settings.json"),
            ):
                WATCHER.write_json(root / "settings.json", {"completion_action": "sleep"})
                WATCHER.set_warning_seconds(120)
            self.assertEqual(
                WATCHER.load_json(root / "settings.json"),
                {"completion_action": "sleep", "warning_seconds": 120},
            )


class WindowsInstallPathTests(unittest.TestCase):
    def test_drive_relative_windows_paths_are_rejected(self) -> None:
        self.assertIsNone(WATCHER.windows_path_to_wsl("C:logs"))
        self.assertEqual(WATCHER.windows_path_to_wsl(r"C:\logs"), Path("/mnt/c/logs"))
        self.assertEqual(WATCHER.windows_path_to_wsl("D:/logs"), Path("/mnt/d/logs"))

    def test_multiple_installations_require_a_matching_windows_profile(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            for username in ("zeta", "alpha"):
                (root / username / "Apps" / "HerdrNachtwaechter").mkdir(parents=True)
            real_path = WATCHER.Path

            def mapped_path(value: object) -> Path:
                return root if value == "/mnt/c/Users" else real_path(value)

            with (
                patch.object(WATCHER, "Path", side_effect=mapped_path),
                patch.dict(
                    WATCHER.os.environ,
                    {"USER": "unknown-profile", "HERDR_NIGHT_WATCH_LOG_DIR": ""},
                    clear=False,
                ),
            ):
                with self.assertRaisesRegex(RuntimeError, "Several HerdrNachtwaechter installations"):
                    WATCHER.default_windows_install_log_dir()

            with (
                patch.object(WATCHER, "Path", side_effect=mapped_path),
                patch.dict(
                    WATCHER.os.environ,
                    {"USER": "alpha", "HERDR_NIGHT_WATCH_LOG_DIR": ""},
                    clear=False,
                ),
            ):
                self.assertEqual(
                    WATCHER.default_windows_install_log_dir(),
                    root / "alpha" / "Apps" / "HerdrNachtwaechter" / "logs",
                )


class ArmingTests(unittest.TestCase):
    def test_explicit_start_accepts_no_current_work(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock")),
                patch.object(WATCHER, "settings_path", return_value=root / "settings.json"),
                patch.object(WATCHER, "verify_herdr"),
                patch.object(WATCHER, "list_agents", return_value=[]),
            ):
                WATCHER.arm(False, 1, 5, 300)
            self.assertEqual(WATCHER.load_json(state_path)["armed_working_count"], 0)


class ConfirmationTests(unittest.TestCase):
    def test_windows_boot_time_marker_parses_round_trip_timestamp(self) -> None:
        for suffix in ("Z", "+02:00", "-05:00", ""):
            parsed = WATCHER.windows_boot_time_from_marker(
                f"wsl:test|windows:2026-08-22T10:00:00.1234567{suffix}"
            )
            self.assertIsNotNone(parsed)
            assert parsed is not None
            self.assertEqual(parsed.microsecond, 123456)
        self.assertEqual(
            WATCHER.windows_boot_time_from_marker(
                "wsl:test|windows:2026-08-22T10:00:00.1234567Z"
            ).tzinfo,
            timezone.utc,
        )
        self.assertIsNone(WATCHER.windows_boot_time_from_marker("wsl:test"))
        self.assertIsNone(
            WATCHER.windows_boot_time_from_marker("wsl:test|windows:not-a-time")
        )

    def test_boot_marker_wait_refuses_when_deadline_is_already_expired(self) -> None:
        with patch.object(WATCHER, "runtime_boot_id") as runtime_boot_id:
            with self.assertRaisesRegex(RuntimeError, "boot marker unavailable"):
                WATCHER.wait_for_runtime_boot_id(0)
        runtime_boot_id.assert_not_called()

    def test_confirmation_refreshes_boot_marker_before_loading_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            WATCHER.write_json(root / "active-run.json", {"run_id": "boot-run", "outcome": None})
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, root / "active-run.json", root / "shutdown-warning.json", root / "watch.lock"),
                ),
                patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current") as wait,
                patch.object(WATCHER, "reset_stale_run_after_restart") as reset,
            ):
                with self.assertRaisesRegex(RuntimeError, "No matching Herdr completion warning"):
                    WATCHER.confirm_completion()
            wait.assert_called_once_with(WATCHER.CONFIRM_BOOT_MARKER_DEADLINE_SECONDS)
            reset.assert_called_once_with(force=True, current_boot_id="boot-current")

    def test_confirmation_boot_refresh_uses_remaining_warning_window(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            now = datetime(2026, 8, 23, 8, 0, tzinfo=timezone.utc)
            WATCHER.write_json(
                state_path,
                {
                    "run_id": "remaining-window-run",
                    "outcome": None,
                    "dry_run": True,
                    "completion_action": "shutdown",
                },
            )
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "remaining-window-run",
                    "completion_action": "shutdown",
                    "cancelable_until": (now + timedelta(seconds=3)).isoformat(),
                },
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "utc_now", return_value=now),
                patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current") as wait,
                patch.object(WATCHER, "reset_stale_run_after_restart") as reset,
            ):
                WATCHER.confirm_completion()
            wait.assert_called_once_with(3.0)
            reset.assert_called_once_with(force=True, current_boot_id="boot-current")

    def test_confirmation_boot_refresh_bounds_invalid_warning_time(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(warning_path, {"cancelable_until": 42})
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, root / "active-run.json", warning_path, root / "watch.lock"),
            ):
                self.assertEqual(
                    WATCHER.confirmation_boot_marker_deadline_seconds(),
                    WATCHER.CONFIRM_BOOT_MARKER_DEADLINE_SECONDS,
                )

    def test_confirmation_does_not_wait_when_warning_window_is_expired(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            now = datetime(2026, 8, 23, 8, 0, tzinfo=timezone.utc)
            WATCHER.write_json(state_path, {"run_id": "expired-run", "outcome": None})
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "expired-run",
                    "completion_action": "shutdown",
                    "cancelable_until": (now - timedelta(seconds=1)).isoformat(),
                },
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "utc_now", return_value=now),
                patch.object(WATCHER, "wait_for_runtime_boot_id") as wait,
            ):
                with self.assertRaisesRegex(RuntimeError, "warning is no longer active"):
                    WATCHER.confirm_completion()
            wait.assert_not_called()

    def test_confirmation_refuses_without_a_matching_warning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            WATCHER.write_json(state_path, {"run_id": "test-run", "outcome": None})
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock"),
            ), patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current"):
                with self.assertRaisesRegex(RuntimeError, "No matching Herdr completion warning"):
                    WATCHER.confirm_completion()

    def test_observation_confirmation_never_calls_windows_power_actions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                state_path,
                {
                    "run_id": "observe-run",
                    "outcome": None,
                    "dry_run": True,
                    "completion_action": "sleep",
                },
            )
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "observe-run",
                    "completion_action": "sleep",
                    "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=60)).isoformat(),
                },
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current"),
                patch.object(WATCHER, "request_windows_sleep") as sleep,
                patch.object(WATCHER, "request_windows_shutdown_now") as shutdown,
            ):
                WATCHER.confirm_completion()
            sleep.assert_not_called()
            shutdown.assert_not_called()
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "dry_run_confirmed")

    def test_cancel_still_aborts_when_history_is_not_utf8(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, {"run_id": "cancel-run", "outcome": None})
            (root / "cancellation-history.csv").write_bytes(b"\xff\xfeinvalid")
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "reset_stale_run_after_restart") as reset,
                patch.object(WATCHER, "abort_shutdown_if_ours", return_value=True) as abort,
            ):
                before = WATCHER.time.monotonic()
                WATCHER.cancel("test")
            reset.assert_called_once()
            reset_kwargs = reset.call_args.kwargs
            self.assertTrue(reset_kwargs["force"])
            self.assertGreaterEqual(reset_kwargs["boot_marker_deadline"], before)
            self.assertLessEqual(
                reset_kwargs["boot_marker_deadline"],
                before + WATCHER.CANCEL_BOOT_MARKER_DEADLINE_SECONDS + 0.1,
            )
            abort.assert_called_once()
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "cancelled")


class ConnectivityTests(unittest.TestCase):
    def test_network_grace_requires_five_uninterrupted_minutes(self) -> None:
        self.assertFalse(WATCHER.network_grace_elapsed({"network_unavailable_since": None}))
        recent = (WATCHER.utc_now() - timedelta(seconds=299)).isoformat()
        self.assertFalse(WATCHER.network_grace_elapsed({"network_unavailable_since": recent}))
        elapsed = (WATCHER.utc_now() - timedelta(seconds=300)).isoformat()
        self.assertTrue(WATCHER.network_grace_elapsed({"network_unavailable_since": elapsed}))

    def test_connectivity_return_clears_the_grace_period(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            state = {"network_unavailable_since": WATCHER.iso_now()}
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock")),
                patch.object(WATCHER, "internet_available", return_value=True),
            ):
                monitor = WATCHER.ConnectivityMonitor()
                self.assertTrue(monitor.refresh(state))
            self.assertIsNone(state["network_unavailable_since"])


class CompletionHistoryTests(unittest.TestCase):
    def setUp(self) -> None:
        original_log_dir = WATCHER.WINDOWS_INSTALL_LOG_DIR
        WATCHER.WINDOWS_INSTALL_LOG_DIR = None
        self.addCleanup(setattr, WATCHER, "WINDOWS_INSTALL_LOG_DIR", original_log_dir)

    def test_history_keeps_only_the_latest_thirty_entries(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with (
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "log"),
            ):
                for index in range(31):
                    WATCHER.record_completion("sleep", "agents_finished", f"run-{index}")
            history = (root / "completion-history.csv").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(history), 31)  # Header plus the latest 30 events.
            self.assertNotIn("run-0", "\n".join(history))
            self.assertIn("run-30", "\n".join(history))

    def test_cancellation_history_records_the_stop_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with (
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "log"),
            ):
                WATCHER.record_cancellation("live_window_moon", "run-test")
            history = (root / "cancellation-history.csv").read_text(encoding="utf-8")
            self.assertIn("live_window_moon", history)
            self.assertIn("run-test", history)

    def test_value_error_while_writing_history_aborts_with_runtime_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with (
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "log"),
                patch.object(csv.DictWriter, "writerows", side_effect=ValueError("bad row")),
            ):
                with self.assertRaisesRegex(RuntimeError, "Completion history could not be persisted"):
                    WATCHER.record_completion("shutdown", "agents_finished", "run-value-error")

    def test_confirmation_history_failure_removes_owned_warning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, {"run_id": "confirm-run", "outcome": None, "dry_run": False})
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "confirm-run",
                    "completion_action": "shutdown",
                    "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=60)).isoformat(),
                },
            )
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, warning_path, root / "watch.lock")),
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current"),
                patch.object(WATCHER, "reset_stale_run_after_restart"),
                patch.object(WATCHER, "record_completion", side_effect=RuntimeError("history unavailable")),
                patch.object(WATCHER.shutil, "which", return_value="/usr/bin/powershell.exe"),
                patch.object(
                    WATCHER.subprocess,
                    "run",
                    return_value=SimpleNamespace(returncode=0, stderr="", stdout=""),
                ) as run,
                patch.object(WATCHER, "log"),
            ):
                with self.assertRaisesRegex(RuntimeError, "history unavailable"):
                    WATCHER.confirm_completion()
            self.assertFalse(warning_path.exists())
            run.assert_not_called()
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "shutdown_failed")

    def test_successful_shutdown_confirmation_persists_before_windows_request(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            state = {"run_id": "confirm-run", "outcome": None, "dry_run": False}
            WATCHER.write_json(state_path, state)
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "confirm-run",
                    "completion_action": "shutdown",
                    "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=60)).isoformat(),
                },
            )
            operations: list[str] = []
            original_finish = WATCHER.finish

            def finish_and_record(current: dict[str, object], outcome: str, detail: str) -> None:
                operations.append("finish")
                original_finish(current, outcome, detail)

            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, warning_path, root / "watch.lock")),
                patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current"),
                patch.object(WATCHER, "reset_stale_run_after_restart"),
                patch.object(
                    WATCHER,
                    "record_completion",
                    side_effect=lambda *_args: operations.append("record"),
                ),
                patch.object(WATCHER, "finish", side_effect=finish_and_record),
                patch.object(
                    WATCHER,
                    "request_windows_shutdown_now",
                    side_effect=lambda: operations.append("shutdown"),
                ),
                patch.object(WATCHER, "log"),
            ):
                self.assertEqual(WATCHER.confirm_completion(), 0)

            self.assertEqual(operations, ["record", "finish", "shutdown"])
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "shutdown_confirmed")
            self.assertFalse(warning_path.exists())


class ShutdownSchedulingTests(unittest.TestCase):
    def state(self) -> dict[str, object]:
        return {
            "run_id": "schedule-run",
            "warning_seconds": 300,
            "completion_action": "shutdown",
            "dry_run": False,
        }

    def test_reset_stale_run_clears_state_from_a_different_boot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, {"run_id": "restart-run", "boot_id": "old-boot"})
            WATCHER.write_json(warning_path, {"run_id": "restart-run"})
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, warning_path, root / "watch.lock")),
                patch.object(WATCHER, "record_cancellation"),
                patch.object(WATCHER, "log"),
            ):
                self.assertTrue(WATCHER.reset_stale_run_after_restart(current_boot_id="new-boot"))
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "reset_after_restart")
            self.assertFalse(warning_path.exists())

    def test_reset_stale_legacy_run_uses_windows_boot_time(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                state_path,
                {"run_id": "legacy-run", "armed_at": "2026-08-22T08:00:00+00:00"},
            )
            WATCHER.write_json(warning_path, {"run_id": "legacy-run"})
            with (
                patch.object(WATCHER, "paths", return_value=(root, state_path, warning_path, root / "watch.lock")),
                patch.object(WATCHER, "record_cancellation"),
                patch.object(WATCHER, "log"),
            ):
                self.assertTrue(
                    WATCHER.reset_stale_run_after_restart(
                        current_boot_id="wsl:test|windows:2026-08-22T10:00:00+00:00"
                    )
                )
            self.assertEqual(WATCHER.load_json(state_path)["reset_reason"], "legacy_state_after_reboot")

    def test_reset_stale_legacy_run_after_boot_time_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            state = {"run_id": "legacy-run", "armed_at": "2026-08-22T12:00:00+00:00"}
            WATCHER.write_json(state_path, state)
            WATCHER.write_json(warning_path, {"run_id": "legacy-run"})
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, state_path, warning_path, root / "watch.lock"),
            ):
                self.assertFalse(
                    WATCHER.reset_stale_run_after_restart(
                        current_boot_id="wsl:test|windows:2026-08-22T10:00:00+00:00"
                    )
                )
            self.assertEqual(WATCHER.load_json(state_path), state)
            self.assertTrue(warning_path.exists())

    def test_shutdown_warning_does_not_start_windows_countdown_early(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER.shutil, "which", return_value="/usr/bin/powershell.exe"),
                patch.object(WATCHER.subprocess, "run") as run,
                patch.object(WATCHER, "log"),
            ):
                WATCHER.schedule_shutdown(self.state())

            run.assert_not_called()
            self.assertTrue(warning_path.exists())

    def test_shutdown_warning_does_not_write_completion_early(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER.shutil, "which", return_value="/usr/bin/powershell.exe"),
                patch.object(WATCHER, "record_completion") as record_completion,
                patch.object(WATCHER.subprocess, "run") as run,
                patch.object(WATCHER, "log"),
            ):
                WATCHER.schedule_shutdown(self.state())

            record_completion.assert_not_called()
            run.assert_not_called()
            self.assertTrue(warning_path.exists())

    def test_missing_powershell_rejects_shutdown_warning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            warning_path = root / "shutdown-warning.json"
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, root / "active-run.json", warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER.shutil, "which", return_value=None),
                patch.object(WATCHER, "log"),
            ):
                with self.assertRaisesRegex(RuntimeError, "powershell.exe is unavailable"):
                    WATCHER.schedule_shutdown(self.state())
            self.assertFalse(warning_path.exists())

    def test_cancel_owned_warning_never_calls_windows(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "schedule-run",
                    "completion_action": "shutdown",
                    "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=60)).isoformat(),
                },
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, root / "active-run.json", warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER.subprocess, "run") as run,
                patch.object(WATCHER, "log"),
            ):
                self.assertTrue(WATCHER.abort_shutdown_if_ours())

            run.assert_not_called()
            self.assertFalse(warning_path.exists())

    def test_abort_without_owned_warning_is_a_noop(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, root / "active-run.json", root / "shutdown-warning.json", root / "watch.lock"),
            ):
                self.assertIsNone(WATCHER.abort_shutdown_if_ours())

    def test_abort_malformed_owned_warning_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            warning_path = root / "shutdown-warning.json"
            warning_path.write_text("{broken", encoding="utf-8")
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, root / "active-run.json", warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "log") as log,
            ):
                self.assertFalse(WATCHER.abort_shutdown_if_ours())

            self.assertTrue(warning_path.exists())
            log.assert_called_once()

    def test_abort_owned_warning_reports_unlink_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                warning_path,
                {
                    "run_id": "schedule-run",
                    "completion_action": "shutdown",
                },
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, root / "active-run.json", warning_path, root / "watch.lock"),
                ),
                patch.object(Path, "unlink", side_effect=PermissionError("denied")),
                patch.object(WATCHER, "log") as log,
            ):
                self.assertFalse(WATCHER.abort_shutdown_if_ours())

            self.assertTrue(warning_path.exists())
            log.assert_called_once()


class WatchWarningRecoveryTests(unittest.TestCase):
    def state(self) -> dict[str, object]:
        return {
            "run_id": "watch-run",
            "poll_seconds": 1,
            "warning_seconds": 1,
            "quiet_seconds": 0,
            "all_terminal_since": "2020-01-01T00:00:00+00:00",
            "completion_action": "shutdown",
            "dry_run": False,
        }

    def patches(self, root: Path, state_path: Path):
        connectivity = SimpleNamespace(refresh=lambda state: False)
        return (
            patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot-current"),
            patch.object(WATCHER, "reset_stale_run_after_restart"),
            patch.object(
                WATCHER,
                "paths",
                return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock"),
            ),
            patch.object(WATCHER, "completion_lock", return_value=nullcontext()),
            patch.object(WATCHER, "ConnectivityMonitor", return_value=connectivity),
            patch.object(WATCHER, "network_grace_elapsed", return_value=False),
            patch.object(WATCHER, "schedule_shutdown"),
            patch.object(WATCHER, "active_run", return_value=self.state()),
            patch.object(WATCHER.time, "sleep"),
            patch.object(WATCHER, "log"),
        )

    def test_successful_shutdown_records_and_finishes_before_windows_request(self) -> None:
        state = self.state()
        state["warning_seconds"] = 0
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, state)
            base = datetime(2026, 8, 31, 22, 26, tzinfo=timezone.utc)
            operations: list[str] = []

            def schedule_warning(current: dict[str, object], reason: str) -> None:
                WATCHER.write_json(
                    warning_path,
                    {
                        "run_id": current["run_id"],
                        "completion_action": "shutdown",
                        "reason": reason,
                        "cancelable_until": base.isoformat(),
                    },
                )

            with ExitStack() as stack:
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "evaluate",
                        side_effect=[("terminal", []), ("terminal", []), ("terminal", [])],
                    )
                )
                stack.enter_context(patch.object(WATCHER, "utc_now", return_value=base))
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "record_completion",
                        side_effect=lambda *_args: operations.append("record"),
                    )
                )
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "request_windows_shutdown_now",
                        side_effect=lambda: operations.append("shutdown"),
                    )
                )
                for context in self.patches(root, state_path):
                    stack.enter_context(context)
                stack.enter_context(
                    patch.object(WATCHER, "schedule_shutdown", side_effect=schedule_warning)
                )
                stack.enter_context(patch.object(WATCHER, "active_run", return_value=state))
                self.assertEqual(WATCHER.watch(), 0)

            self.assertEqual(operations, ["record", "shutdown"])
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "shutdown_scheduled")
            self.assertFalse(warning_path.exists())

    def test_warning_abort_failure_fails_closed_without_completion_action(self) -> None:
        state = self.state()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            base = datetime(2026, 8, 23, tzinfo=timezone.utc)
            with ExitStack() as stack:
                stack.enter_context(patch.object(WATCHER, "load_json", side_effect=[state, state]))
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "evaluate",
                        side_effect=[("terminal", []), ("terminal", []), ("active", ["p=working"])],
                    )
                )
                abort = stack.enter_context(patch.object(WATCHER, "abort_shutdown_or_fail", return_value=False))
                execute = stack.enter_context(patch.object(WATCHER, "request_windows_shutdown_now"))
                record = stack.enter_context(patch.object(WATCHER, "record_completion"))
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "utc_now",
                        side_effect=[base, base, base, base + timedelta(seconds=2)],
                    )
                )
                for context in self.patches(root, state_path):
                    stack.enter_context(context)
                self.assertEqual(WATCHER.watch(), 1)
            abort.assert_called_once()
            execute.assert_not_called()
            record.assert_not_called()

    def test_successful_warning_abort_resets_quiet_period_and_resumes_monitoring(self) -> None:
        state = self.state()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            WATCHER.write_json(state_path, state)
            base = datetime(2026, 8, 23, tzinfo=timezone.utc)
            with ExitStack() as stack:
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "load_json",
                        side_effect=[state, state, {"run_id": "watch-run", "outcome": "cancelled"}],
                    )
                )
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "evaluate",
                        side_effect=[("terminal", []), ("terminal", []), ("active", ["p=working"])],
                    )
                )
                abort = stack.enter_context(patch.object(WATCHER, "abort_shutdown_or_fail", return_value=True))
                execute = stack.enter_context(patch.object(WATCHER, "request_windows_shutdown_now"))
                record = stack.enter_context(patch.object(WATCHER, "record_completion"))
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "utc_now",
                        side_effect=[base, base, base, base + timedelta(seconds=2)],
                    )
                )
                for context in self.patches(root, state_path):
                    stack.enter_context(context)
                self.assertEqual(WATCHER.watch(), 0)
            abort.assert_called_once()
            execute.assert_not_called()
            record.assert_not_called()
            self.assertIsNone(WATCHER.load_json(state_path)["all_terminal_since"])

    def test_warning_removed_before_abort_fails_closed_in_watch(self) -> None:
        state = self.state()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, state)
            evaluations = 0

            def evaluate_and_remove_warning(_state: dict[str, object]) -> tuple[str, list[str]]:
                nonlocal evaluations
                evaluations += 1
                if evaluations == 3:
                    warning_path.unlink(missing_ok=True)
                    return "active", ["p=working"]
                return "terminal", []

            def schedule_warning(_state: dict[str, object], _reason: str) -> None:
                WATCHER.write_json(
                    warning_path,
                    {
                        "run_id": state["run_id"],
                        "completion_action": "shutdown",
                        "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=60)).isoformat(),
                    },
                )

            with ExitStack() as stack:
                stack.enter_context(patch.object(WATCHER, "evaluate", side_effect=evaluate_and_remove_warning))
                stack.enter_context(patch.object(WATCHER, "schedule_shutdown", side_effect=schedule_warning))
                stack.enter_context(patch.object(WATCHER, "abort_shutdown_if_ours", wraps=WATCHER.abort_shutdown_if_ours))
                stack.enter_context(patch.object(WATCHER, "abort_shutdown_or_fail", wraps=WATCHER.abort_shutdown_or_fail))
                for context in self.patches(root, state_path):
                    stack.enter_context(context)
                self.assertEqual(WATCHER.watch(), 1)
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "shutdown_abort_failed")

    def test_sleep_failure_clears_owned_warning_before_reraising(self) -> None:
        state = self.state()
        state["completion_action"] = "sleep"
        state["warning_seconds"] = 0
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(state_path, state)
            base = datetime(2026, 8, 23, tzinfo=timezone.utc)

            def schedule_warning(current: dict[str, object], reason: str) -> None:
                WATCHER.write_json(
                    warning_path,
                    {
                        "run_id": current["run_id"],
                        "completion_action": "sleep",
                        "cancelable_until": (base + timedelta(seconds=60)).isoformat(),
                    },
                )

            with ExitStack() as stack:
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "evaluate",
                        side_effect=[("terminal", []), ("terminal", []), ("terminal", [])],
                    )
                )
                stack.enter_context(
                    patch.object(
                        WATCHER,
                        "utc_now",
                        side_effect=[base, base, base, base, base],
                    )
                )
                stack.enter_context(
                    patch.object(WATCHER, "record_completion", side_effect=RuntimeError("history unavailable"))
                )
                sleep = stack.enter_context(patch.object(WATCHER, "request_windows_sleep"))
                abort = stack.enter_context(
                    patch.object(WATCHER, "abort_shutdown_if_ours", wraps=WATCHER.abort_shutdown_if_ours)
                )
                for context in self.patches(root, state_path):
                    stack.enter_context(context)
                stack.enter_context(patch.object(WATCHER, "active_run", return_value=state))
                stack.enter_context(patch.object(WATCHER, "schedule_shutdown", side_effect=schedule_warning))
                with self.assertRaisesRegex(RuntimeError, "history unavailable"):
                    WATCHER.watch()
            sleep.assert_not_called()
            abort.assert_called_once()
            self.assertFalse(warning_path.exists())
            self.assertEqual(WATCHER.load_json(state_path)["outcome"], "sleep_failed")


class DiagnosticLogTests(unittest.TestCase):
    def test_diagnostics_are_machine_readable_and_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, root / "active-run.json", root / "shutdown-warning.json", root / "watch.lock"),
            ):
                for index in range(WATCHER.DIAGNOSTIC_HISTORY_LIMIT + 1):
                    WATCHER.record_diagnostic(f"STATUS event={index}", sequence=index)
            entries = [
                json.loads(line)
                for line in (root / "diagnostics.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(entries), WATCHER.DIAGNOSTIC_HISTORY_LIMIT)
            self.assertEqual(entries[0]["sequence"], 1)
            self.assertEqual(entries[-1]["sequence"], WATCHER.DIAGNOSTIC_HISTORY_LIMIT)
            self.assertEqual(entries[-1]["event"], "STATUS")


if __name__ == "__main__":
    unittest.main()
