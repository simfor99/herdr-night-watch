"""Bounded, versioned status-only pipe server; never starts a night run."""
from __future__ import annotations

import contextlib
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone, timedelta

VERSION = 1
MAX_FRAME = 256 * 1024
REQUEST_SECONDS = 25.0  # Leaves five seconds for client I/O and cleanup.
_children: set[subprocess.Popen] = set()


class CleanupFailed(BaseException):
    pass


class ChannelClosed(BaseException):
    pass


def remaining(deadline: float, cap: float) -> float:
    seconds = min(cap, deadline - time.monotonic())
    if seconds <= 0:
        raise TimeoutError("Status deadline exceeded")
    return seconds


def reap(child: subprocess.Popen) -> None:
    """Clean the dedicated command group, including descendants after parent exit."""
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        child.communicate(timeout=0.3)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        child.communicate(timeout=2)
    except subprocess.TimeoutExpired as error:
        raise CleanupFailed("Status child cleanup not confirmed") from error
    _children.discard(child)


def run_bounded(arguments: list[str], deadline: float) -> subprocess.CompletedProcess:
    # Block termination during registration; otherwise EOF could orphan a child
    # in the gap between Popen returning and adding it to our cleanup set.
    blocked = {signal.SIGTERM, signal.SIGALRM}
    old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, blocked)
    try:
        # Reset the inherited mask in a single-threaded exec wrapper. Never use
        # preexec_fn in this multi-threaded transport process.
        wrapper = "import os,signal,sys;signal.pthread_sigmask(signal.SIG_SETMASK,[]);os.execvp(sys.argv[1],sys.argv[1:])"
        child = subprocess.Popen([sys.executable, "-c", wrapper, *arguments], text=True, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, stdin=subprocess.DEVNULL,
                                 start_new_session=True)
        _children.add(child)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
    try:
        stdout, stderr = child.communicate(timeout=remaining(deadline, 5))
        return subprocess.CompletedProcess(arguments, child.returncode, stdout, stderr)
    finally:
        reap(child)


def _alarm(_signum, _frame):
    raise TimeoutError("Status request timed out")


def _closed(_signum, _frame):
    raise ChannelClosed()


@contextlib.contextmanager
def request_budget(watcher):
    previous = watcher._STATUS_DEADLINE
    watcher._STATUS_DEADLINE = time.monotonic() + REQUEST_SECONDS
    signal.setitimer(signal.ITIMER_REAL, REQUEST_SECONDS)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        watcher._STATUS_DEADLINE = previous


def serve(watcher) -> int:
    """One request at a time, with immediate EOF cancellation even during work."""
    ticks = int(os.environ["HERDR_STATUS_WINDOWS_BOOT_TICKS"])
    if ticks <= 0:
        raise ValueError("Missing native Windows boot timestamp")
    boot = datetime(1601, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=ticks // 10)
    # Match existing .NET round-trip marker exactly, including seventh digit.
    watcher._STATUS_WINDOWS_BOOT_ID = boot.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ticks % 10_000_000:07d}Z"
    transport = sys.stdout
    incoming: queue.Queue = queue.Queue(maxsize=1)
    stop = threading.Event()
    main_pid = os.getpid()
    old_term = signal.signal(signal.SIGTERM, _closed)
    old_alarm = signal.signal(signal.SIGALRM, _alarm)

    def cancel():
        if not stop.is_set():
            os.kill(main_pid, signal.SIGTERM)

    def reader():
        try:
            while not stop.is_set():
                raw = sys.stdin.buffer.readline(MAX_FRAME + 1)
                if not raw:
                    cancel()
                    return
                if len(raw) > MAX_FRAME or not raw.endswith(b"\n"):
                    cancel()
                    return
                # Queue saturation is a protocol violation, never an unbounded backlog.
                try:
                    incoming.put_nowait(raw)
                except queue.Full:
                    cancel()
                    return
        except (OSError, ValueError):
            cancel()

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    sequence = 0
    try:
        while True:
            raw = incoming.get()
            request_id = None
            try:
                request = json.loads(raw)
                if (not isinstance(request, dict) or request.get("version") != VERSION
                        or request.get("op") != "status"
                        or type(request.get("id")) is not int or request["id"] < 0):
                    raise ValueError("Unsupported status protocol request")
                request_id = request["id"]
                with request_budget(watcher), contextlib.redirect_stdout(sys.stderr):
                    payload = watcher.status_payload()
                sequence += 1
                response = {"version": VERSION, "id": request_id, "sequence": sequence,
                            "generated_at": datetime.now(timezone.utc).isoformat(),
                            "generated_unix_ms": int(time.time() * 1000),
                            "status": payload}
            except Exception as error:
                response = {"version": VERSION, "id": request_id,
                            "error": f"{type(error).__name__}: {error}"}
            frame = json.dumps(response, separators=(",", ":"))
            if len(frame.encode()) > MAX_FRAME:
                frame = json.dumps({"version": VERSION, "id": request_id,
                                    "error": "Status response exceeds frame limit"})
            transport.write(frame + "\n")
            transport.flush()
    except (ChannelClosed, BrokenPipeError):
        return 0
    finally:
        stop.set()
        signal.setitimer(signal.ITIMER_REAL, 0)
        # Prevent a second EOF signal from interrupting mandatory cleanup.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        for child in list(_children):
            reap(child)
        try:
            transport.write(json.dumps({"version": VERSION, "closed": True}) + "\n")
            transport.flush()
        except (BrokenPipeError, OSError):
            pass
        signal.signal(signal.SIGTERM, old_term)
        signal.signal(signal.SIGALRM, old_alarm)
