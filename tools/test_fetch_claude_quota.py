from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import fetch_claude_quota as claude_quota


class ClaudeQuotaCaptureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.state_home = Path(self.temporary_directory.name)
        self.environment = patch.dict(os.environ, {"XDG_STATE_HOME": str(self.state_home)})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.addCleanup(self.temporary_directory.cleanup)
        self.now = 1_790_000_000

    def test_capture_persists_only_valid_limit_fields_with_private_permissions(self) -> None:
        payload = {
            "session_id": "must-not-be-copied",
            "transcript_path": "/private/conversation.jsonl",
            "model": {"display_name": "Claude"},
            "rate_limits": {
                "five_hour": {"used_percentage": 32.4, "resets_at": self.now + 1800},
                "seven_day": {"used_percentage": 4, "resets_at": self.now + 86400},
                "spend_limit": {"used_percentage": 99, "resets_at": self.now + 100},
            },
        }

        self.assertTrue(claude_quota.capture_statusline_payload(payload, now=self.now))
        path = claude_quota.state_path()
        stored = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(
            stored,
            {
                "captured_at": self.now,
                "rate_limits": {
                    "five_hour": {
                        "used_percentage": 32.4,
                        "resets_at": self.now + 1800,
                    },
                    "week": {"used_percentage": 4, "resets_at": self.now + 86400},
                },
            },
        )
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("must-not-be-copied", path.read_text(encoding="utf-8"))

    def test_missing_invalid_or_expired_windows_are_not_reported(self) -> None:
        payload = {
            "rate_limits": {
                "five_hour": {"used_percentage": 32, "resets_at": self.now - 1},
                "seven_day": {"used_percentage": True, "resets_at": self.now + 600},
            }
        }
        self.assertFalse(claude_quota.capture_statusline_payload(payload, now=self.now))
        self.assertIsNone(claude_quota.read_quota(now=self.now))

    def test_reader_rejects_old_or_future_cache_and_drops_reset_windows(self) -> None:
        payload = {
            "rate_limits": {
                "five_hour": {"used_percentage": 50, "resets_at": self.now + 600},
                "seven_day": {"used_percentage": 20, "resets_at": self.now + 3600},
            }
        }
        claude_quota.capture_statusline_payload(payload, now=self.now)

        self.assertEqual(
            claude_quota.read_quota(now=self.now + 601),
            {"week": {"used_percentage": 20, "resets_at": self.now + 3600}},
        )
        self.assertIsNone(claude_quota.read_quota(now=self.now + 3601))
        self.assertIsNone(claude_quota.read_quota(now=self.now + claude_quota.MAX_AGE_SECONDS + 1))
        self.assertIsNone(claude_quota.read_quota(now=self.now - 1))


if __name__ == "__main__":
    unittest.main()
