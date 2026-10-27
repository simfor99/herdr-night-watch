"""Completion safety regressions; every Windows power action is mocked."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


WATCHER = load("guard_watcher", "herdr-night-watch.py")
FIRSTMATE = load("firstmate_guard", "firstmate_work_guard.py")


class WorkGuardTests(unittest.TestCase):
    def guard(self, status):
        return {"id": "workflow", "command": [sys.executable, "-c",
            "import json; print(json.dumps(" + repr({"schema_version": 1, "status": status}) + "))"]}

    def test_active_workflow_blocks_completion_even_when_claude_finished_a_turn(self):
        state = {"monitoring_scope": "live_agents", "work_guards": [self.guard("active")]}
        with patch.object(WATCHER, "verify_herdr"), patch.object(WATCHER, "list_agents", return_value=[{"pane_id": "p1", "agent_status": "done"}]):
            self.assertEqual(WATCHER.evaluate(state)[0], "active")

    def test_released_workflow_still_requires_all_herdr_agents_to_finish(self):
        state = {"monitoring_scope": "live_agents", "work_guards": [self.guard("ready")]}
        with patch.object(WATCHER, "verify_herdr"), patch.object(WATCHER, "list_agents", return_value=[{"pane_id": "p1", "agent_status": "working"}]):
            self.assertEqual(WATCHER.evaluate(state)[0], "active")
        with patch.object(WATCHER, "verify_herdr"), patch.object(WATCHER, "list_agents", return_value=[{"pane_id": "p1", "agent_status": "done"}]):
            self.assertEqual(WATCHER.evaluate(state)[0], "terminal")

    def test_unknown_workflow_refuses_completion(self):
        self.assertEqual(WATCHER.evaluate_work_guards({"work_guards": [self.guard("unknown")]})[0], "refuse")

    def test_failed_timed_out_and_invalid_guards_never_allow_completion(self):
        for guard in [self.guard("invalid"), {"id": "missing", "command": ["/nonexistent/work-guard"]}]:
            with self.assertRaises(RuntimeError):
                WATCHER.evaluate_work_guards({"work_guards": [guard]})
        with patch.object(WATCHER.subprocess, "run", side_effect=subprocess.TimeoutExpired("guard", 5)):
            self.assertFalse(WATCHER.work_guards_ready({"work_guards": [self.guard("ready")]}))

    def test_firstmate_task_and_unread_wake_block_but_idle_observers_do_not(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            (home / "bin").mkdir()
            (home / "bin/fm-supervision-lib.sh").touch()
            (home / "state/procevent").mkdir(parents=True)
            (home / "state/procevent/observer.source").touch()
            (home / "state/.last-watcher-beat").touch()
            self.assertEqual(FIRSTMATE.pending_work(home)["status"], "ready")
            task = home / "state/task.meta"
            task.write_text("herdr_pane_id=p1\n")
            self.assertEqual(FIRSTMATE.pending_work(home)["status"], "active")
            task.unlink()
            (home / "state/.wake-queue").write_text("pending continuation\n")
            self.assertEqual(FIRSTMATE.pending_work(home)["status"], "active")
            (home / "state/.wake-queue").write_text("")
            self.assertEqual(FIRSTMATE.pending_work(home)["status"], "ready")
            with patch.object(Path, "iterdir", side_effect=PermissionError("unreadable state")):
                self.assertEqual(FIRSTMATE.pending_work(home)["status"], "unknown")

    def test_missing_firstmate_state_is_unknown(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(FIRSTMATE.pending_work(Path(temporary))["status"], "unknown")

    def test_guard_config_is_frozen_at_arm_and_bad_config_refuses_arm(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / "work-guards.json"
            config.write_text(json.dumps({"schema_version": 1, "guards": [self.guard("active")]}))
            with patch.object(WATCHER, "state_dir", return_value=root), patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot"), patch.object(WATCHER, "reset_stale_run_after_restart"), patch.object(WATCHER, "verify_herdr"), patch.object(WATCHER, "list_agents", return_value=[]), patch.object(WATCHER, "log"):
                WATCHER.arm(True, 1, 5, 300)
                config.unlink()
                state = WATCHER.load_json(root / "active-run.json")
                self.assertEqual(WATCHER.evaluate_work_guards(state)[0], "active")
                config.write_text('{"schema_version": 1, "guards": [{"id":"bad", "command":"shell string"}]}')
                with self.assertRaises(RuntimeError):
                    WATCHER.load_work_guard_config()


class WarningSafetyTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.state_path = self.root / "active-run.json"
        self.warning_path = self.root / "shutdown-warning.json"
        self.state = {"run_id": "run", "outcome": None, "dry_run": False, "monitoring_scope": "live_agents", "targets": []}
        self.warning = {"run_id": "run", "warning_id": "warning-a", "completion_action": "shutdown", "reason": "agents_finished", "cancelable_until": (WATCHER.utc_now() + timedelta(seconds=300)).isoformat()}
        WATCHER.write_json(self.state_path, self.state)
        WATCHER.write_json(self.warning_path, self.warning)
        self.stack.enter_context(patch.object(WATCHER, "paths", return_value=(self.root, self.state_path, self.warning_path, self.root / "watch.lock")))
        self.stack.enter_context(patch.object(WATCHER, "wait_for_runtime_boot_id", return_value="boot"))
        self.stack.enter_context(patch.object(WATCHER, "reset_stale_run_after_restart"))
        self.stack.enter_context(patch.object(WATCHER, "log"))
        self.shutdown = self.stack.enter_context(patch.object(WATCHER, "request_windows_shutdown_now"))
        self.sleep = self.stack.enter_context(patch.object(WATCHER, "request_windows_sleep"))
        self.record = self.stack.enter_context(patch.object(WATCHER, "record_completion"))

    def assert_no_power(self):
        self.shutdown.assert_not_called()
        self.sleep.assert_not_called()
        self.record.assert_not_called()

    def test_ok_rechecks_busy_and_uncertain_agents_and_removes_warning(self):
        for result in ["active", "refuse"]:
            WATCHER.write_json(self.warning_path, self.warning)
            with patch.object(WATCHER, "evaluate", return_value=(result, ["p1=working"])) as evaluate:
                with self.assertRaisesRegex(RuntimeError, "Completion refused"):
                    WATCHER.confirm_completion("warning-a")
            evaluate.assert_called_once()
            self.assertFalse(self.warning_path.exists())
            self.assertIsNone(WATCHER.load_json(self.state_path)["all_terminal_since"])
        self.assert_no_power()

    def test_ok_rechecks_pending_workflow_even_when_all_panes_report_done(self):
        self.state["work_guards"] = [WorkGuardTests().guard("active")]
        WATCHER.write_json(self.state_path, self.state)
        with patch.object(WATCHER, "verify_herdr"), patch.object(WATCHER, "list_agents", return_value=[{"pane_id": "p1", "agent_status": "done"}]):
            with self.assertRaisesRegex(RuntimeError, "Completion refused"):
                WATCHER.confirm_completion("warning-a")
        self.assert_no_power()

    def test_ok_fails_closed_on_check_error(self):
        with patch.object(WATCHER, "evaluate", side_effect=RuntimeError("status unavailable")):
            with self.assertRaisesRegex(RuntimeError, "status unavailable"):
                WATCHER.confirm_completion("warning-a")
        self.assertFalse(self.warning_path.exists())
        self.assert_no_power()

    def test_rejected_ok_returns_the_existing_watcher_to_monitoring(self):
        self.state.update(poll_seconds=1, warning_seconds=60, quiet_seconds=0,
                          all_terminal_since="2020-01-01T00:00:00+00:00")
        WATCHER.write_json(self.state_path, self.state)
        sleeps = []

        def schedule(state, reason):
            WATCHER.write_json(self.warning_path, self.warning)
            return "warning-a"

        def poll(_seconds):
            sleeps.append(True)
            if len(sleeps) == 1:
                with self.assertRaisesRegex(RuntimeError, "Completion refused"):
                    WATCHER.confirm_completion("warning-a")
            else:
                state = WATCHER.load_json(self.state_path)
                state["outcome"] = "cancelled"
                WATCHER.write_json(self.state_path, state)

        with patch.object(WATCHER, "ConnectivityMonitor", return_value=SimpleNamespace(refresh=lambda state: True)), patch.object(WATCHER, "network_grace_elapsed", return_value=False), patch.object(WATCHER, "evaluate", side_effect=[("terminal", []), ("terminal", []), ("active", []), ("active", [])]), patch.object(WATCHER, "schedule_shutdown", side_effect=schedule) as scheduled, patch.object(WATCHER.time, "sleep", side_effect=poll):
            self.assertEqual(WATCHER.watch(), 0)
        self.assertEqual(len(sleeps), 2)
        scheduled.assert_called_once()
        self.assertFalse(self.warning_path.exists())
        self.assertEqual(WATCHER.load_json(self.state_path)["withdrawn_warning_id"], "warning-a")
        self.assert_no_power()

    def test_old_dialog_cannot_confirm_or_cancel_a_new_warning(self):
        for operation in [lambda: WATCHER.confirm_completion("old-warning"), lambda: WATCHER.cancel("warning_dialog", "old-warning")]:
            with self.assertRaises(RuntimeError):
                operation()
            self.assertEqual(WATCHER.load_json(self.warning_path)["warning_id"], "warning-a")
            self.assertIsNone(WATCHER.load_json(self.state_path)["outcome"])
        self.assert_no_power()

    def test_lightweight_warning_identity_tracks_cancellation_and_expiry(self):
        self.assertEqual(WATCHER.current_warning_id(), "warning-a")
        with patch.object(WATCHER, "utc_now", return_value=WATCHER.utc_now() + timedelta(seconds=301)):
            self.assertIsNone(WATCHER.current_warning_id())
        self.warning_path.unlink()
        self.assertIsNone(WATCHER.current_warning_id())

    def test_only_fresh_ready_decision_can_request_power(self):
        with patch.object(WATCHER, "evaluate", return_value=("terminal", [])) as evaluate:
            self.assertEqual(WATCHER.confirm_completion("warning-a"), 0)
        evaluate.assert_called_once()
        self.shutdown.assert_called_once()
        self.sleep.assert_not_called()
        self.record.assert_called_once()

    def test_network_warning_also_respects_pending_workflow(self):
        self.warning["reason"] = "network_unavailable"
        WATCHER.write_json(self.warning_path, self.warning)
        with patch.object(WATCHER, "work_guards_ready", return_value=False), patch.object(WATCHER, "internet_available", return_value=False):
            with self.assertRaisesRegex(RuntimeError, "Completion refused"):
                WATCHER.confirm_completion("warning-a")
        self.assert_no_power()


if __name__ == "__main__":
    unittest.main()
