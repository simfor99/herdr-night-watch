from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

import fetch_qwen_quota as qwen_quota


class QwenWindowMappingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.now = time.time()

    def test_consumed_fraction_becomes_remaining_percent(self) -> None:
        mapped = qwen_quota.window(0.013, int((self.now + 86400) * 1000))
        self.assertIsNotNone(mapped)
        self.assertEqual(mapped["remaining_percent"], 99)
        # The unrounded fraction rides along so the widget pace is not built
        # from integer-percent noise in the first hours of a cycle.
        self.assertAlmostEqual(mapped["consumed_percent"], 1.3, places=3)

    def test_invalid_or_expired_windows_are_rejected(self) -> None:
        self.assertIsNone(qwen_quota.window(1.5, int((self.now + 1000) * 1000)))
        self.assertIsNone(qwen_quota.window("x", int((self.now + 1000) * 1000)))
        self.assertIsNone(qwen_quota.window(0.1, int((self.now - 10) * 1000)))


class QwenUsagePipelineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary_directory.name)
        environment = patch.dict(os.environ, {"HOME": str(self.home)})
        environment.start()
        self.addCleanup(environment.stop)
        self.addCleanup(self.temporary_directory.cleanup)

    def test_only_reported_windows_are_emitted_with_cycle_length(self) -> None:
        usage = {
            "per1MonthPercentage": 0.2451,
            "per1MonthResetTime": int((time.time() + 30 * 86400) * 1000),
        }
        subscription = {
            "startTime": int((time.time() - 86400) * 1000),
            "endTime": int((time.time() + 30 * 86400) * 1000),
        }

        def fake_gateway(cookie, sec_token, endpoint):
            return usage if endpoint == "usage" else subscription

        fake_request = lambda *args, **kwargs: b'{"data": {"secToken": "test-token"}}'
        with patch.object(qwen_quota, "request", fake_request), patch.object(
            qwen_quota, "gateway_call", side_effect=fake_gateway
        ):
            result = qwen_quota.usage_with_cookie("cookie")

        self.assertIn("month", result)
        self.assertNotIn("five_hour", result)
        self.assertEqual(result["month"]["remaining_percent"], 75)
        self.assertTrue(28.0 <= result["month"]["cycle_days"] <= 32.0)

    def test_subscription_cycle_never_stamps_the_week_window(self) -> None:
        # A ~31-day subscription length describing a rolling 7-day week window
        # would age the cycle artificially and quiet real bottlenecks.
        usage = {
            "per1WeekPercentage": 0.5,
            "per1WeekResetTime": int((time.time() + 3 * 86400) * 1000),
        }
        subscription = {
            "startTime": int((time.time() - 30 * 86400) * 1000),
            "endTime": int((time.time() + 1 * 86400) * 1000),
        }

        def fake_gateway(cookie, sec_token, endpoint):
            return usage if endpoint == "usage" else subscription

        fake_request = lambda *args, **kwargs: b'{"data": {"secToken": "test-token"}}'
        with patch.object(qwen_quota, "request", fake_request), patch.object(
            qwen_quota, "gateway_call", side_effect=fake_gateway
        ):
            result = qwen_quota.usage_with_cookie("cookie")

        self.assertIn("week", result)
        self.assertNotIn("cycle_days", result["week"])

    def test_usage_endpoint_failure_yields_no_result(self) -> None:
        fake_request = lambda *args, **kwargs: b'{"data": {"secToken": "test-token"}}'
        with patch.object(qwen_quota, "request", fake_request), patch.object(
            qwen_quota, "gateway_call", return_value=None
        ):
            self.assertIsNone(qwen_quota.usage_with_cookie("cookie"))

    def test_missing_sec_token_never_reaches_the_gateway(self) -> None:
        fake_request = lambda *args, **kwargs: b'{"data": {}}'
        with patch.object(qwen_quota, "request", fake_request), patch.object(
            qwen_quota, "gateway_call"
        ) as gateway:
            self.assertIsNone(qwen_quota.usage_with_cookie("cookie"))
            gateway.assert_not_called()

    def test_cdp_read_stops_at_the_global_budget(self) -> None:
        # A slow-drip peer that keeps trickling bytes inside each per-recv
        # timeout must not extend the read past the promised global budget.
        class DripSocket:
            def settimeout(self, timeout: float) -> None:
                pass

            def recv(self, _size: int) -> bytes:
                return b"x"

        client = qwen_quota.CdpSocket.__new__(qwen_quota.CdpSocket)
        client._sock = DripSocket()
        client._buffer = b""
        # Simulated clock: each arm consumes five seconds of budget.
        remaining = iter([15.0, 10.0, 5.0, 1.0, 0.0])
        with patch.object(qwen_quota, "budget_left", lambda: next(remaining, 0.0)):
            with self.assertRaises(OSError):
                client._read(16)

    def test_cookie_file_round_trip_is_private_and_bounded(self) -> None:
        qwen_quota.write_cookie_file("cna=x; login_qwencloud_ticket=y")
        path = Path(qwen_quota.cookie_file_path())
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(qwen_quota.read_cookie_file(), "cna=x; login_qwencloud_ticket=y")

        # A seven-hour-old cache is past the six-hour freshness bound and is
        # refused, while explicit readers still see the stored value.
        os.utime(path, (time.time() - 7 * 3600, time.time() - 7 * 3600))
        self.assertIsNone(qwen_quota.read_cookie_file())
        self.assertEqual(
            qwen_quota.read_cookie_file(max_age=False),
            "cna=x; login_qwencloud_ticket=y",
        )


if __name__ == "__main__":
    unittest.main()
