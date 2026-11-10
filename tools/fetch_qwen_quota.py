#!/usr/bin/env python3
"""Read QwenCloud Token Plan quota windows and print a JSON snapshot.

The `sk-sp-*` inference key used by ccqwen is routing-only: every billing
path on the token-plan host 404s and responses carry no rate-limit headers
(verified 2026-11-04, mirrors steipete/CodexBar#2328). Quota data therefore
comes from the QwenCloud console gateway, which authenticates via browser
cookies.

Cookie sources, tried in order (values are never printed or logged):
  1. QWEN_CLOUD_COOKIE environment variable (raw Cookie header value)
  2. ~/.config/herdr-night-watch/qwen-cookies.txt (single line, written by
     the CDP refresh below or pasted manually)
  3. The agent Chrome's remote debugging endpoint (Chrome-Debug-Profile,
     --remote-debugging-port=9222): Storage.getCookies pulls the HttpOnly
     session cookies and persists them for the pure-HTTP path afterwards.

The cookie file is tried first regardless of its age; the CDP refresh and
its rewrite of the file only run when authentication fails, and a CDP cookie
is written back only after it provably returned usage data. A manually
exported session therefore stays usable until it genuinely stops working
(verified 2026-11-06: during the QwenCloud maintenance outage the debug
profile held a dead ticket that would have clobbered a live manual export
under the old 6-hour age gate).

Output on stdout: {"five_hour"|"month"|"week": {"remaining_percent": int,
"consumed_percent": float, "reset_epoch": int}, ...}; the month window
additionally carries cycle_days. The live Token Plan payload is polled for
per5Hour*, per1Month* and per1Week* fields; only windows the account really
reports are emitted. Fail-closed: missing, expired or rejected credentials
exit non-zero without stdout, so the card shows no stale values.
"""

from __future__ import annotations

import base64
import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

GATEWAY = "https://cs-data.qwencloud.com/data/api.json"
SEC_TOKEN_URL = "https://home.qwencloud.com/tool/user/info.json"
API_PREFIX = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/"
BROWSER_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36"
)
HTTP_TIMEOUT_SECONDS = 10
# Global wall-clock budget across every step (cookie attempts, CDP refresh,
# retries). The widget refreshes all providers sequentially every 45 s; an
# unbounded Qwen failure must never delay the other cards.
TOTAL_BUDGET_SECONDS = 20
_BUDGET_START = time.monotonic()


def budget_left() -> float:
    return TOTAL_BUDGET_SECONDS - (time.monotonic() - _BUDGET_START)


def out_of_budget() -> bool:
    return budget_left() <= 1.0


CDP_PORT = 9222
# Cookie names the console session needs (per CodexBar#2328 findings plus the
# region/risk hints this gateway returns alongside them).
COOKIE_ALLOWLIST = {
    "login_qwencloud_ticket",
    "login_aliyunid_pk",
    "cna",
    "isg",
    "maas-region",
    "sca",
    "yunpk",
}


def cookie_file_path() -> str:
    return os.path.expanduser("~/.config/herdr-night-watch/qwen-cookies.txt")


def read_cookie_file() -> str | None:
    path = cookie_file_path()
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line and not line.startswith("#"):
                    return line
    except OSError:
        return None
    return None


def write_cookie_file(cookie: str) -> None:
    path = cookie_file_path()
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    temporary = f"{path}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(cookie + "\n")
        os.replace(temporary, path)
    except OSError:
        try:
            os.unlink(temporary)
        except OSError:
            pass


def cdp_hosts() -> list[str]:
    hosts: list[str] = []
    configured = os.environ.get("HERDR_QWEN_CDP_HOST", "").strip()
    if configured:
        hosts.append(configured)
    hosts.append("127.0.0.1")
    try:
        route = subprocess.run(
            ["ip", "route"], capture_output=True, text=True, timeout=3, check=False
        )
        for line in route.stdout.splitlines():
            parts = line.split()
            if parts[:1] == ["default"]:
                hosts.append(parts[2])
    except (OSError, subprocess.SubprocessError, IndexError):
        pass
    return list(dict.fromkeys(hosts))


def http_json(url: str) -> object | None:
    if out_of_budget():
        return None
    try:
        with urllib.request.urlopen(url, timeout=min(3.0, max(0.5, budget_left()))) as response:
            return json.loads(response.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError):
        return None


def cdp_find_socket() -> str | None:
    """Return a page's webSocketDebuggerUrl from a reachable CDP endpoint."""
    for host in cdp_hosts():
        targets = http_json(f"http://{host}:{CDP_PORT}/json/list")
        if not isinstance(targets, list):
            continue
        for target in targets:
            if not isinstance(target, dict) or target.get("type") != "page":
                continue
            endpoint = target.get("webSocketDebuggerUrl")
            # Chrome advertises ws://127.0.0.1:9222/... no matter which host
            # answered; rebuild the URL against the host we reached.
            if isinstance(endpoint, str) and "/devtools/page/" in endpoint:
                suffix = endpoint[endpoint.index("/devtools/page/") :]
                return f"ws://{host}:{CDP_PORT}{suffix}"
    return None


class CdpSocket:
    """Minimal stdlib WebSocket client for one CDP request/response pair.

    Every blocking read re-arms the socket timeout from the global budget, so
    a slow-drip peer cannot keep the process alive past the 20 s promise the
    Rust caller relies on for its sequential all-provider refresh.
    """

    def _arm_timeout(self) -> float:
        remaining = budget_left()
        if remaining <= 0.0:
            raise OSError("CDP read exceeded the global budget")
        timeout = min(8.0, remaining)
        self._sock.settimeout(timeout)
        return timeout

    def __init__(self, host: str, port: int, path: str) -> None:
        self._sock = socket.create_connection(
            (host, port), timeout=max(0.5, min(8.0, budget_left()))
        )
        key = base64.b64encode(os.urandom(16)).decode()
        handshake = (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self._sock.sendall(handshake.encode())
        self._buffer = b""
        while b"\r\n\r\n" not in self._buffer:
            self._arm_timeout()
            chunk = self._sock.recv(4096)
            if not chunk:
                raise OSError("CDP handshake closed")
            self._buffer += chunk
        header, self._buffer = self._buffer.split(b"\r\n\r\n", 1)
        if b"101" not in header.split(b"\r\n", 1)[0]:
            raise OSError("CDP handshake rejected")

    def _read(self, size: int) -> bytes:
        while len(self._buffer) < size:
            self._arm_timeout()
            chunk = self._sock.recv(65536)
            if not chunk:
                raise OSError("CDP socket closed")
            self._buffer += chunk
        out, self._buffer = self._buffer[:size], self._buffer[size:]
        return out

    def send(self, payload: dict) -> None:
        data = json.dumps(payload).encode()
        header = bytearray([0x81])
        length = len(data)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        mask = os.urandom(4)
        self._sock.sendall(bytes(header) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def recv_json(self, want_id: int, deadline: float) -> dict | None:
        while time.time() < deadline:
            first = self._read(2)
            opcode = first[0] & 0x0F
            length = first[1] & 0x7F
            if length == 126:
                length = struct.unpack(">H", self._read(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self._read(8))[0]
            payload = self._read(length)
            if opcode == 0x8:
                return None
            if opcode != 0x1:
                continue
            try:
                message = json.loads(payload)
            except json.JSONDecodeError:
                continue
            if isinstance(message, dict) and message.get("id") == want_id:
                return message
        return None

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass


def cookies_via_cdp() -> str | None:
    if out_of_budget():
        return None
    endpoint = cdp_find_socket()
    if endpoint is None:
        return None
    parsed = urllib.parse.urlsplit(endpoint)
    if parsed.scheme != "ws" or parsed.hostname is None:
        return None
    try:
        client = CdpSocket(parsed.hostname, parsed.port or CDP_PORT, parsed.path)
    except OSError:
        return None
    try:
        client.send({"id": 1, "method": "Storage.getCookies"})
        answer = client.recv_json(1, time.time() + max(0.5, min(8.0, budget_left())))
    finally:
        client.close()
    if not isinstance(answer, dict):
        return None
    cookies = answer.get("result", {}).get("cookies")
    if not isinstance(cookies, list):
        return None
    pairs: list[str] = []
    for cookie in cookies:
        if not isinstance(cookie, dict):
            continue
        name = cookie.get("name")
        value = cookie.get("value")
        domain = str(cookie.get("domain", ""))
        if (
            isinstance(name, str)
            and name in COOKIE_ALLOWLIST
            and isinstance(value, str)
            and value
            and ("qwencloud" in domain or "aliyun" in domain)
        ):
            pairs.append(f"{name}={value}")
    return "; ".join(pairs) or None


def request(url: str, cookie: str, data: bytes | None = None) -> bytes:
    if out_of_budget():
        raise TimeoutError("qwen fetch budget exhausted")
    headers = {
        "Cookie": cookie,
        "User-Agent": BROWSER_UA,
        "Origin": "https://home.qwencloud.com",
        "Referer": "https://home.qwencloud.com/billing/subscription/token-plan-individual",
    }
    if data is not None:
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    req = urllib.request.Request(url, data=data, headers=headers)
    timeout = max(0.5, min(HTTP_TIMEOUT_SECONDS, budget_left()))
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return response.read()


def gateway_call(cookie: str, sec_token: str, endpoint: str) -> dict | None:
    api = API_PREFIX + endpoint
    params = {
        "Api": api,
        "Data": {"cornerstoneParam": {
            "domain": "home.qwencloud.com",
            "consoleSite": "QWENCLOUD",
            "console": "ONE_CONSOLE",
            "xsp_lang": "en-US",
            "protocol": "V2",
            "productCode": "p_efm",
        }},
    }
    body = urllib.parse.urlencode({
        "product": "sfm_bailian",
        "action": "IntlBroadScopeAspnGateway",
        "sec_token": sec_token,
        "region": "ap-southeast-1",
        "V": "1.0",
        "params": json.dumps(params),
    }).encode()
    query = urllib.parse.urlencode({
        "product": "sfm_bailian",
        "action": "IntlBroadScopeAspnGateway",
        "api": api,
    })
    try:
        payload = json.loads(request(f"{GATEWAY}?{query}", cookie, body))
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError, OSError):
        return None
    data = payload
    for key in ("data", "DataV2", "data", "data"):
        if not isinstance(data, dict):
            return None
        data = data.get(key)
    return data if isinstance(data, dict) else None


def window(used_fraction: object, reset_ms: object) -> dict | None:
    if not isinstance(used_fraction, (int, float)) or isinstance(used_fraction, bool):
        return None
    if not 0.0 <= float(used_fraction) <= 1.0:
        return None
    if not isinstance(reset_ms, (int, float)) or isinstance(reset_ms, bool):
        return None
    epoch = int(reset_ms) // 1000
    if epoch <= int(time.time()):
        return None
    return {
        "remaining_percent": max(0, min(100, round(100 - float(used_fraction) * 100))),
        # Exact (unrounded) consumption: integer percents multiplied by a tiny
        # cycle age would otherwise dominate the fresh-cycle pace estimate.
        "consumed_percent": round(float(used_fraction) * 100, 4),
        "reset_epoch": epoch,
    }


def usage_with_cookie(cookie: str) -> dict | None:
    try:
        info = json.loads(request(SEC_TOKEN_URL, cookie))
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError, OSError):
        return None
    sec_token = None
    if isinstance(info, dict):
        data = info.get("data")
        if isinstance(data, dict):
            sec_token = data.get("secToken")
    if not isinstance(sec_token, str) or not sec_token:
        return None

    usage = gateway_call(cookie, sec_token, "usage")
    if usage is None:
        return None

    result: dict[str, dict] = {}
    for key, used, reset in (
        ("five_hour", usage.get("per5HourPercentage"), usage.get("per5HourResetTime")),
        ("month", usage.get("per1MonthPercentage"), usage.get("per1MonthResetTime")),
        ("week", usage.get("per1WeekPercentage"), usage.get("per1WeekResetTime")),
    ):
        mapped = window(used, reset)
        if mapped is not None:
            result[key] = mapped
    if not result:
        return None

    # The subscription window tells us the true cycle length. Without it the
    # widget would have to guess how much of the period has already elapsed
    # and invent a burn-rate pace for freshly started cycles.
    subscription = gateway_call(cookie, sec_token, "subscription")
    if isinstance(subscription, dict):
        start = subscription.get("startTime")
        end = subscription.get("endTime")
        if (
            isinstance(start, (int, float))
            and not isinstance(start, bool)
            and isinstance(end, (int, float))
            and not isinstance(end, bool)
            and end > start
        ):
            cycle_days = round((end - start) / 86_400_000.0, 2)
            # The subscription length only describes the monthly window. A
            # rolling week window must keep the 7-day pacing guess; stamping
            # ~31 days onto it would age the cycle artificially and quiet a
            # real bottleneck.
            if "month" in result:
                result["month"]["cycle_days"] = cycle_days
    return result


def main() -> int:
    # Hard watchdog: no blocking HTTP or CDP read may outlive the promised
    # budget; the Rust caller waits for this process and refreshes every
    # provider sequentially. The timer fires at the exact end of the global
    # budget (measured from module import), not main() entry plus grace.
    # Fail closed with no stdout.
    watchdog = threading.Timer(max(0.1, budget_left()), lambda: os._exit(1))
    watchdog.daemon = True
    watchdog.start()
    cookie = os.environ.get("QWEN_CLOUD_COOKIE", "").strip() or read_cookie_file()
    result = usage_with_cookie(cookie) if cookie else None
    if result is None:
        # The cached or supplied cookie did not authenticate (or the request
        # failed): refresh it from the agent browser session once and retry
        # immediately. Only a CDP cookie that demonstrably produced usage is
        # persisted; otherwise a dead debug-profile ticket would clobber a
        # still-live manual export during a provider outage (2026-11-06).
        fresh = cookies_via_cdp()
        if fresh:
            result = usage_with_cookie(fresh)
            if result:
                write_cookie_file(fresh)
    if not result:
        return 1
    json.dump(result, sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
