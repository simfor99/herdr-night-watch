from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
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

    def test_watchdog_hard_stops_a_blocking_run(self) -> None:
        # A peer that keeps the connection open past the global budget must
        # take the whole process down at the budget deadline - including time
        # already spent before main() (the pre-fix grace timer survived that).
        module_dir = os.path.dirname(os.path.abspath(qwen_quota.__file__))
        lines = [
            "import sys, time",
            f"sys.path.insert(0, {module_dir!r})",
            "import fetch_qwen_quota as q",
            # 16 of the 20 budget seconds were already spent before main().
            "q._BUDGET_START = time.monotonic() - 16.0",
            "import socket",
            "def slow_connect(self, address, timeout=None):",
            "    time.sleep(30)",
            "    raise OSError('connect blocked')",
            "socket.socket.connect = slow_connect",
            "q.main()",
        ]
        code = "\n".join(lines)
        env = {**os.environ, "HOME": str(self.home), "QWEN_CLOUD_COOKIE": ""}
        start = time.monotonic()
        result = subprocess.run(
            [sys.executable, "-c", code], capture_output=True, timeout=40, env=env
        )
        elapsed = time.monotonic() - start
        # Four budget seconds remain at main() start, so the watchdog must
        # kill the process at ~4 s - well before the 30 s block ends. The old
        # grace formula (budget + 2 s counted from main entry) would have
        # slept the full 30 s and only then exited.
        self.assertLess(elapsed, 20.0, f"watchdog let the run last {elapsed:.1f}s")
        self.assertEqual(result.returncode, 1, result.stderr[-400:].decode(errors="replace"))

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

    def test_cookie_file_round_trip_is_private_and_ageless(self) -> None:
        qwen_quota.write_cookie_file("cna=x; login_qwencloud_ticket=y")
        path = Path(qwen_quota.cookie_file_path())
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(qwen_quota.read_cookie_file(), "cna=x; login_qwencloud_ticket=y")

        # A manually exported session stays readable after seven hours. The
        # old six-hour age gate discarded live manual cookies and let the CDP
        # refresh overwrite them with the debug profile's dead ticket during
        # the 2026-11-06 QwenCloud outage.
        os.utime(path, (time.time() - 7 * 3600, time.time() - 7 * 3600))
        self.assertEqual(qwen_quota.read_cookie_file(), "cna=x; login_qwencloud_ticket=y")

    def test_main_tries_cookie_file_of_any_age_before_cdp(self) -> None:
        qwen_quota.write_cookie_file("login_qwencloud_ticket=manual")
        os.utime(
            Path(qwen_quota.cookie_file_path()),
            (time.time() - 7 * 3600, time.time() - 7 * 3600),
        )
        seen: list[str] = []

        def fake_usage(cookie: str) -> dict | None:
            seen.append(cookie)
            return {
                "week": {
                    "remaining_percent": 42,
                    "reset_epoch": int(time.time()) + 3600,
                    "consumed_percent": 58.0,
                }
            }

        def forbidden_cdp_refresh() -> None:
            raise AssertionError("CDP must not run while the stored cookie authenticates")

        with patch.dict(os.environ, {"QWEN_CLOUD_COOKIE": ""}), patch.object(
            qwen_quota, "usage_with_cookie", side_effect=fake_usage
        ), patch.object(qwen_quota, "cookies_via_cdp", forbidden_cdp_refresh):
            self.assertEqual(qwen_quota.main(), 0)
        self.assertEqual(seen, ["login_qwencloud_ticket=manual"])
        self.assertEqual(qwen_quota.read_cookie_file(), "login_qwencloud_ticket=manual")

    def test_main_refreshes_via_cdp_only_after_file_auth_fails(self) -> None:
        qwen_quota.write_cookie_file("login_qwencloud_ticket=stale")
        attempts: list[str] = []

        def fake_usage(cookie: str) -> dict | None:
            attempts.append(cookie)
            if cookie == "login_qwencloud_ticket=fresh":
                return {
                    "week": {
                        "remaining_percent": 42,
                        "reset_epoch": int(time.time()) + 3600,
                        "consumed_percent": 58.0,
                    }
                }
            return None

        with patch.dict(os.environ, {"QWEN_CLOUD_COOKIE": ""}), patch.object(
            qwen_quota, "usage_with_cookie", side_effect=fake_usage
        ), patch.object(
            qwen_quota, "cookies_via_cdp", lambda: "login_qwencloud_ticket=fresh"
        ):
            self.assertEqual(qwen_quota.main(), 0)
        self.assertEqual(
            attempts, ["login_qwencloud_ticket=stale", "login_qwencloud_ticket=fresh"]
        )
        self.assertEqual(qwen_quota.read_cookie_file(), "login_qwencloud_ticket=fresh")


if __name__ == "__main__":
    unittest.main()
