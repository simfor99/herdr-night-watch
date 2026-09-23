import datetime as dt
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fetch_codex_quota import fetch_quota, latest_quota, normalize_windows


class CodexQuotaTest(unittest.TestCase):
    def test_app_server_reports_week_without_five_hour_window(self):
        now = dt.datetime(2026, 9, 23, 18, 20, tzinfo=dt.timezone.utc)
        limits = {
            "primary": {"usedPercent": 3, "windowDurationMins": 10080, "resetsAt": 1790790254},
            "secondary": None,
        }
        self.assertEqual(normalize_windows(limits, now, app_server=True)["week"]["remaining_percent"], 97)
        self.assertNotIn("five_hour", normalize_windows(limits, now, app_server=True))

    def test_recent_observation_and_expired_reset(self):
        now = dt.datetime(2026, 9, 23, 18, 20, tzinfo=dt.timezone.utc)
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory)
            day = sessions / "2026/09/23"
            day.mkdir(parents=True)
            events = [
                {
                    "timestamp": "2026-09-23T18:10:00Z",
                    "payload": {
                        "type": "token_count",
                        "rate_limits": {
                            "limit_id": "codex",
                            "primary": {"used_percent": 2.0, "window_minutes": 10080, "resets_at": 1790790254},
                            "secondary": {"used_percent": 100.0, "window_minutes": 300, "resets_at": 1790187000},
                        },
                    },
                }
            ]
            (day / "rollout-test.jsonl").write_text(json.dumps(events[0]) + "\n", encoding="utf-8")

            result = latest_quota(sessions, now)
            self.assertEqual(result["week"]["remaining_percent"], 98)
            self.assertNotIn("five_hour", result)

            self.assertIsNone(latest_quota(sessions, now + dt.timedelta(days=2)))

    def test_windows_are_identified_by_duration(self):
        now = dt.datetime(2026, 9, 23, 18, 20, tzinfo=dt.timezone.utc)
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory)
            day = sessions / "2026/09/23"
            day.mkdir(parents=True)
            event = {
                "timestamp": "2026-09-23T18:10:00Z",
                "payload": {
                    "type": "token_count",
                    "rate_limits": {
                        "limit_id": "codex",
                        "primary": {"used_percent": 25, "window_minutes": 300, "resets_at": 1790190000},
                        "secondary": {"used_percent": 40, "window_minutes": 10080, "resets_at": 1790790254},
                    },
                },
            }
            (day / "rollout-test.jsonl").write_text(json.dumps(event) + "\n", encoding="utf-8")

            result = latest_quota(sessions, now)
            self.assertEqual(result["five_hour"]["remaining_percent"], 75)
            self.assertRegex(result["five_hour"]["reset"], r"^\d{2}:\d{2}$")
            self.assertEqual(result["week"]["remaining_percent"], 60)

    def test_unreadable_candidate_metadata_does_not_abort_scan(self):
        now = dt.datetime(2026, 9, 23, 18, 20, tzinfo=dt.timezone.utc)
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory)
            day = sessions / "2026/09/23"
            day.mkdir(parents=True)
            event = {
                "timestamp": "2026-09-23T18:10:00Z",
                "payload": {"type": "token_count", "rate_limits": {
                    "limit_id": "codex",
                    "primary": {"used_percent": 10, "window_minutes": 300, "resets_at": 1790190000},
                }},
            }
            (day / "rollout-test.jsonl").write_text(json.dumps(event) + "\n", encoding="utf-8")
            original_stat = Path.stat

            def unreadable_file_stat(path, *args, **kwargs):
                if path.name == "rollout-test.jsonl":
                    raise OSError("file metadata disappeared")
                return original_stat(path, *args, **kwargs)

            with patch.object(Path, "stat", unreadable_file_stat):
                result = latest_quota(sessions, now)
            self.assertEqual(result["five_hour"]["remaining_percent"], 90)

    def test_app_server_exception_falls_back_to_session_log(self):
        now = dt.datetime(2026, 9, 23, 18, 20, tzinfo=dt.timezone.utc)
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory)
            day = sessions / "2026/09/23"
            day.mkdir(parents=True)
            event = {
                "timestamp": "2026-09-23T18:10:00Z",
                "payload": {"type": "token_count", "rate_limits": {
                    "limit_id": "codex",
                    "primary": {"used_percent": 10, "window_minutes": 300, "resets_at": 1790190000},
                }},
            }
            (day / "rollout-test.jsonl").write_text(json.dumps(event) + "\n", encoding="utf-8")
            with patch("fetch_codex_quota.app_server_quota", side_effect=RuntimeError("app-server failed")):
                result = fetch_quota(sessions, now)
            self.assertEqual(result["five_hour"]["remaining_percent"], 90)


if __name__ == "__main__":
    unittest.main()
