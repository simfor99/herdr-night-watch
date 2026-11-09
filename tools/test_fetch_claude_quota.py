from __future__ import annotations

import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import fetch_claude_quota as claude_quota


class FakeResponse(io.BytesIO):
    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *args: object) -> bool:
        return False


class ClaudeQuotaCaptureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.state_home = Path(self.temporary_directory.name)
        self.environment = patch.dict(os.environ, {"XDG_STATE_HOME": str(self.state_home)})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.addCleanup(self.temporary_directory.cleanup)
        # Deterministisch: ohne echten OAuth-Fallback in den Capture-Tests.
        oauth_off = patch.object(claude_quota, "fetch_oauth_windows", return_value=None)
        oauth_off.start()
        self.addCleanup(oauth_off.stop)
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


class ClaudeQuotaOauthFallbackTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.state_home = Path(self.temporary_directory.name)
        self.credentials_dir = Path(self.temporary_directory.name) / "claude"
        self.credentials_dir.mkdir()
        environment = patch.dict(
            os.environ, {"XDG_STATE_HOME": str(self.state_home)}
        )
        environment.start()
        self.addCleanup(environment.stop)
        credentials_patch = patch.object(
            claude_quota,
            "oauth_credentials_path",
            return_value=self.credentials_dir / ".credentials.json",
        )
        credentials_patch.start()
        self.addCleanup(credentials_patch.stop)
        self.addCleanup(self.temporary_directory.cleanup)
        self.now = 1_790_000_000

    def write_credentials(self, token: str | None, expires_at: object) -> None:
        oauth: dict[str, object] = {"expiresAt": expires_at}
        if token is not None:
            oauth["accessToken"] = token
        (self.credentials_dir / ".credentials.json").write_text(
            json.dumps({"claudeAiOauth": oauth}), encoding="utf-8"
        )

    def test_token_is_only_accepted_while_valid(self) -> None:
        self.write_credentials("fresh-token", int((self.now + 3600) * 1000))
        self.assertEqual(claude_quota.oauth_access_token(self.now), "fresh-token")

        self.write_credentials("expired-token", int((self.now - 1) * 1000))
        self.assertIsNone(claude_quota.oauth_access_token(self.now))

        (self.credentials_dir / ".credentials.json").unlink()
        self.assertIsNone(claude_quota.oauth_access_token(self.now))

    def test_usage_payload_maps_windows_without_leaking_the_token(self) -> None:
        usage = {
            "five_hour": None,
            "seven_day": {
                "utilization": 100.0,
                "resets_at": "2026-11-04T20:00:00.131246+00:00",
            },
        }
        captured_headers: dict[str, str] = {}

        def fake_urlopen(request, timeout=None):
            captured_headers.update(dict(request.headers))
            return FakeResponse(json.dumps(usage).encode())

        with patch.object(
            claude_quota, "oauth_access_token", return_value="fresh-token"
        ), patch.object(claude_quota.urllib.request, "urlopen", fake_urlopen):
            windows = claude_quota.fetch_oauth_windows(self.now)

        self.assertEqual(
            windows,
            {
                "week": {
                    "used_percentage": 100.0,
                    "resets_at": 1_793_822_400,
                }
            },
        )
        self.assertEqual(
            captured_headers.get("Authorization"), "Bearer fresh-token"
        )

    def test_expired_reset_windows_are_dropped(self) -> None:
        usage = {"seven_day": {"utilization": 3.0, "resets_at": "2020-01-01T00:00:00+00:00"}}

        def fake_urlopen(request, timeout=None):
            return FakeResponse(json.dumps(usage).encode())

        with patch.object(
            claude_quota, "oauth_access_token", return_value="fresh-token"
        ), patch.object(claude_quota.urllib.request, "urlopen", fake_urlopen):
            self.assertIsNone(claude_quota.fetch_oauth_windows(self.now))

    def test_read_falls_back_to_oauth_and_caches_beyond_the_short_ttl(self) -> None:
        fallback = patch.object(
            claude_quota,
            "fetch_oauth_windows",
            return_value={"week": {"used_percentage": 42.0, "resets_at": self.now + 7200}},
        )
        with fallback as mocked:
            windows = claude_quota.read_quota(now=self.now)
            self.assertEqual(windows, {"week": {"used_percentage": 42.0, "resets_at": self.now + 7200}})
            self.assertEqual(mocked.call_count, 1)

            # Kurz-Cache: direkt danach wird nicht erneut netzgeholt.
            cached = claude_quota.read_quota(now=self.now + 60)
            self.assertEqual(cached, windows)
            self.assertEqual(mocked.call_count, 1)

            # Nach der OAuth-TTL holt der Reader frisch.
            claude_quota.read_quota(now=self.now + claude_quota.OAUTH_MAX_AGE_SECONDS + 1)
            self.assertEqual(mocked.call_count, 2)

        stored = json.loads(claude_quota.oauth_state_path().read_text(encoding="utf-8"))
        # Der letzte Refresh nach TTL-Ablauf hat den Zeitstempel erneuert.
        self.assertEqual(
            stored.get("captured_at"),
            self.now + claude_quota.OAUTH_MAX_AGE_SECONDS + 1,
        )
        self.assertNotIn("fresh-token", json.dumps(stored))

    def test_drained_week_from_oauth_overrides_fresh_statusline_capture(self) -> None:
        # Regressionsfall 2026-11-09: Die Karte zeigte stundenlang "2% übrig"
        # aus einem frischen Status-Line-Capture (98% verbraucht), obwohl das
        # Wochenlimit bereits aufgebraucht war und der offizielle Endpoint
        # 100% utilization meldete - aufgebrauchte Limits stoppen genau die
        # Sessions, die neue Captures liefern würden.
        claude_quota.capture_statusline_payload(
            {
                "rate_limits": {
                    "seven_day": {"used_percentage": 98, "resets_at": self.now + 86400}
                }
            },
            now=self.now,
        )
        drained = patch.object(
            claude_quota,
            "fetch_oauth_windows",
            return_value={"week": {"used_percentage": 100.0, "resets_at": self.now + 86400}},
        )
        with drained:
            self.assertEqual(
                claude_quota.read_quota(now=self.now + 60),
                {"week": {"used_percentage": 100.0, "resets_at": self.now + 86400}},
            )

    def test_oauth_failure_falls_back_to_fresh_statusline_capture(self) -> None:
        claude_quota.capture_statusline_payload(
            {
                "rate_limits": {
                    "seven_day": {"used_percentage": 98, "resets_at": self.now + 86400}
                }
            },
            now=self.now,
        )
        with patch.object(claude_quota, "fetch_oauth_windows", return_value=None):
            self.assertEqual(
                claude_quota.read_quota(now=self.now + 60),
                {"week": {"used_percentage": 98, "resets_at": self.now + 86400}},
            )

    def test_statusline_capture_does_not_touch_the_oauth_cache(self) -> None:
        with patch.object(
            claude_quota,
            "fetch_oauth_windows",
            return_value={"week": {"used_percentage": 100.0, "resets_at": self.now + 86400}},
        ):
            claude_quota.read_quota(now=self.now)
        before = claude_quota.oauth_state_path().read_text(encoding="utf-8")

        claude_quota.capture_statusline_payload(
            {
                "rate_limits": {
                    "seven_day": {"used_percentage": 55, "resets_at": self.now + 86400}
                }
            },
            now=self.now + 30,
        )

        self.assertEqual(
            claude_quota.oauth_state_path().read_text(encoding="utf-8"), before
        )
        stored = json.loads(claude_quota.state_path().read_text(encoding="utf-8"))
        self.assertEqual(
            stored.get("rate_limits"),
            {"week": {"used_percentage": 55, "resets_at": self.now + 86400}},
        )


if __name__ == "__main__":
    unittest.main()
