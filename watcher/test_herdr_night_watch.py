#!/usr/bin/env python3
"""Focused safety tests for the Herdr night watcher's decision function."""

from __future__ import annotations

import importlib.util
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from datetime import timedelta


SCRIPT = Path(__file__).with_name("herdr-night-watch.py")
SPEC = importlib.util.spec_from_file_location("herdr_night_watch", SCRIPT)
assert SPEC and SPEC.loader
WATCHER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WATCHER)


def agent(pane_id: str, status: str | None) -> dict[str, object]:
    return {"pane_id": pane_id, "agent_status": status}


LIVE_STATE = {"demo": False, "monitoring_scope": "live_agents", "targets": []}
PROJECT_ROOT = Path(__file__).resolve().parent.parent


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


class WindowsLauncherTests(unittest.TestCase):
    def test_scheduled_task_uses_the_powershell_launcher(self) -> None:
        launcher = (PROJECT_ROOT / "windows" / "Run-HerdrNightWatchHidden.ps1").read_text(encoding="utf-8")
        installer = (PROJECT_ROOT / "windows" / "Install-HerdrNightWatch.ps1").read_text(encoding="utf-8")

        self.assertIn("Start-Process", launcher)
        self.assertIn("-NoNewWindow", launcher)
        self.assertIn("'/usr/bin/python3'", launcher)
        self.assertIn("Run-HerdrNightWatchHidden.ps1", installer)
        self.assertIn("WindowsPowerShell", installer)
        self.assertNotIn("wscript.exe", installer)

    def test_tray_starts_the_watcher_without_a_scheduled_task(self) -> None:
        backend = (PROJECT_ROOT / "src" / "backend.rs").read_text(encoding="utf-8")

        start_body = backend[backend.index("pub fn start"):backend.index("pub fn demo")]
        demo_body = backend[backend.index("pub fn demo"):backend.index("pub fn stop")]
        self.assertIn("spawn_watcher()", start_body)
        self.assertIn("spawn_watcher()", demo_body)
        self.assertNotIn("schtasks.exe", start_body)
        self.assertNotIn("schtasks.exe", demo_body)

    def test_tray_never_spawns_reg_exe(self) -> None:
        sources = "\n".join(path.read_text(encoding="utf-8") for path in (PROJECT_ROOT / "src").glob("*.rs"))

        self.assertNotIn('Command::new("reg.exe")', sources)

    def test_power_guard_smoke_test_has_activation_and_release_checks(self) -> None:
        smoke_test = (PROJECT_ROOT / "windows" / "Test-HerdrNightWatchPowerGuard.ps1").read_text(encoding="utf-8")

        self.assertIn("SetThreadExecutionState", smoke_test)
        self.assertIn("powercfg.exe", smoke_test)
        self.assertIn("/requests", smoke_test)
        self.assertIn("WindowsPrincipal", smoke_test)
        self.assertIn("$esContinuous", smoke_test)
        self.assertIn("[Convert]::ToUInt32('80000000', 16)", smoke_test)
        self.assertIn("finally", smoke_test)

    def test_tray_session_marker_records_unclean_exit_in_tray_history(self) -> None:
        history = (PROJECT_ROOT / "src" / "tray_history.rs").read_text(encoding="utf-8")
        log_viewer = (PROJECT_ROOT / "src" / "log_viewer.rs").read_text(encoding="utf-8")

        self.assertIn('"tray-session.json"', history)
        self.assertIn('"tray-history.csv"', history)
        self.assertIn("expected_history_tail", history)
        self.assertIn("Tray-App unplanmäßig beendet", history)
        self.assertIn('directory.join("completion-history.csv")', log_viewer)
        self.assertIn('directory.join("tray-history.csv")', log_viewer)

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


class WarningResetTests(unittest.TestCase):
    def test_work_resuming_during_warning_requires_a_new_quiet_period(self) -> None:
        class StopWatch(Exception):
            pass

        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            state = {
                "run_id": "warning-reset-run",
                "boot_id": "test-boot",
                "outcome": None,
                "demo": False,
                "dry_run": True,
                "monitoring_scope": WATCHER.LIVE_MONITORING_SCOPE,
                "poll_seconds": 1,
                "quiet_seconds": 1,
                "warning_seconds": 2,
                "completion_action": "shutdown",
                "all_terminal_since": (WATCHER.utc_now() - timedelta(seconds=10)).isoformat(),
                "network_unavailable_since": None,
            }
            WATCHER.write_json(state_path, state)
            evaluate_results = iter(
                [
                    ("terminal", ["live_agents=0"]),
                    ("terminal", ["live_agents=0"]),
                    ("active", ["p1=working"]),
                    ("terminal", ["live_agents=0"]),
                ]
            )
            sleep_calls = 0

            def controlled_sleep(_seconds: float) -> None:
                nonlocal sleep_calls
                sleep_calls += 1
                if sleep_calls >= 2:
                    raise StopWatch()

            def abort_warning() -> bool:
                warning_path.unlink(missing_ok=True)
                return True

            with (
                patch.object(WATCHER, "reset_stale_run_after_restart", return_value=False),
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "evaluate", side_effect=lambda _state: next(evaluate_results)),
                patch.object(WATCHER.time, "sleep", side_effect=controlled_sleep),
                patch.object(WATCHER, "abort_shutdown_if_ours", side_effect=abort_warning) as abort,
            ):
                with self.assertRaises(StopWatch):
                    WATCHER.watch()

            self.assertEqual(abort.call_count, 1)
            updated = WATCHER.load_json(state_path)
            self.assertIsNone(updated["outcome"])
            self.assertIsNotNone(updated["all_terminal_since"])
            self.assertFalse(warning_path.exists())

    def test_final_warning_check_aborts_before_action_when_work_resumes(self) -> None:
        class StopWatch(Exception):
            pass

        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            base_time = WATCHER.utc_now()
            current_time = [base_time]
            state = {
                "run_id": "final-check-run",
                "boot_id": "test-boot",
                "outcome": None,
                "demo": False,
                "dry_run": True,
                "monitoring_scope": WATCHER.LIVE_MONITORING_SCOPE,
                "poll_seconds": 1,
                "quiet_seconds": 1,
                "warning_seconds": 1,
                "completion_action": "shutdown",
                "all_terminal_since": (base_time - timedelta(seconds=10)).isoformat(),
                "network_unavailable_since": None,
            }
            WATCHER.write_json(state_path, state)
            evaluate_results = iter(
                [
                    ("terminal", ["live_agents=0"]),
                    ("terminal", ["live_agents=0"]),
                    ("terminal", ["live_agents=0"]),
                    ("active", ["p1=working"]),
                    ("terminal", ["live_agents=0"]),
                ]
            )
            sleep_calls = 0

            def fake_now() -> object:
                return current_time[0]

            def controlled_sleep(seconds: float) -> None:
                nonlocal sleep_calls
                sleep_calls += 1
                current_time[0] += timedelta(seconds=seconds)
                if sleep_calls >= 2:
                    raise StopWatch()

            def abort_warning() -> bool:
                warning_path.unlink(missing_ok=True)
                return True

            with (
                patch.object(WATCHER, "reset_stale_run_after_restart", return_value=False),
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "utc_now", side_effect=fake_now),
                patch.object(WATCHER, "evaluate", side_effect=lambda _state: next(evaluate_results)),
                patch.object(WATCHER.time, "sleep", side_effect=controlled_sleep),
                patch.object(WATCHER, "abort_shutdown_if_ours", side_effect=abort_warning) as abort,
            ):
                with self.assertRaises(StopWatch):
                    WATCHER.watch()

            self.assertEqual(abort.call_count, 1)
            updated = WATCHER.load_json(state_path)
            self.assertIsNone(updated["outcome"])
            self.assertIsNotNone(updated["all_terminal_since"])
            self.assertFalse(warning_path.exists())


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


class RestartResetTests(unittest.TestCase):
    def setUp(self) -> None:
        WATCHER._RUNTIME_BOOT_ID = None

    def test_runtime_boot_id_includes_windows_boot_marker(self) -> None:
        with (
            patch.object(WATCHER, "wsl_boot_id", return_value="wsl-boot"),
            patch.object(WATCHER, "windows_boot_id", return_value="2026-08-15T05:00:00Z"),
        ):
            self.assertEqual(
                WATCHER.runtime_boot_id(),
                "wsl:wsl-boot|windows:2026-08-15T05:00:00Z",
            )

    def test_runtime_boot_id_requires_both_boot_markers(self) -> None:
        with (
            patch.object(WATCHER, "wsl_boot_id", return_value="wsl-boot"),
            patch.object(WATCHER, "windows_boot_id", return_value=None),
        ):
            self.assertIsNone(WATCHER.runtime_boot_id())

    def test_active_run_is_not_reset_when_current_boot_marker_is_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                state_path,
                {"run_id": "same-run", "boot_id": "old-boot", "outcome": None},
            )
            WATCHER.write_json(warning_path, {"run_id": "same-run"})
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.log"),
                ),
                patch.object(WATCHER, "runtime_boot_id", return_value=None),
            ):
                self.assertFalse(WATCHER.reset_stale_run_after_restart())
            self.assertIsNone(WATCHER.load_json(state_path)["outcome"])
            self.assertTrue(warning_path.exists())

    def test_active_run_is_reset_after_a_new_boot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            warning_path = root / "shutdown-warning.json"
            WATCHER.write_json(
                state_path,
                {
                    "run_id": "old-run",
                    "boot_id": "old-boot",
                    "outcome": None,
                },
            )
            WATCHER.write_json(warning_path, {"run_id": "old-run"})
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "runtime_boot_id", return_value="new-boot"),
                patch.object(WATCHER, "record_cancellation"),
                patch.object(WATCHER, "log"),
            ):
                self.assertTrue(WATCHER.reset_stale_run_after_restart())
            state = WATCHER.load_json(state_path)
            self.assertEqual(state["outcome"], "reset_after_restart")
            self.assertEqual(state["reset_reason"], "boot_id_changed")
            self.assertFalse(warning_path.exists())

    def test_active_run_survives_normal_watcher_restart(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            WATCHER.write_json(
                state_path,
                {"run_id": "same-run", "boot_id": "same-boot", "outcome": None},
            )
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock"),
                ),
                patch.object(WATCHER, "runtime_boot_id", return_value="same-boot"),
            ):
                self.assertFalse(WATCHER.reset_stale_run_after_restart())
            self.assertIsNone(WATCHER.load_json(state_path)["outcome"])


class ConfirmationTests(unittest.TestCase):
    def test_confirmation_refuses_without_a_matching_warning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            state_path = root / "active-run.json"
            WATCHER.write_json(
                state_path,
                {"run_id": "test-run", "boot_id": WATCHER.runtime_boot_id(), "outcome": None},
            )
            with patch.object(
                WATCHER,
                "paths",
                return_value=(root, state_path, root / "shutdown-warning.json", root / "watch.lock"),
            ):
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
                    "boot_id": WATCHER.runtime_boot_id(),
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
            WATCHER.write_json(
                state_path,
                {"run_id": "cancel-run", "boot_id": WATCHER.runtime_boot_id(), "outcome": None},
            )
            (root / "cancellation-history.csv").write_bytes(b"\xff\xfeinvalid")
            with (
                patch.object(
                    WATCHER,
                    "paths",
                    return_value=(root, state_path, warning_path, root / "watch.lock"),
                ),
                patch.object(WATCHER, "WINDOWS_INSTALL_LOG_DIR", root),
                patch.object(WATCHER, "abort_shutdown_if_ours", return_value=True) as abort,
            ):
                WATCHER.cancel("test")
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
