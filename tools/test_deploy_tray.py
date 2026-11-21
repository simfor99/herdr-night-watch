#!/usr/bin/env python3
"""Deterministic tests for the overlapping Windows power guard in deploy_tray.py.

Covers the handover invariants from the AC work order: the guard must be
active before Stop-Process runs, must stay confirmed alive across the whole
handover (helper death after the sentinel fails closed), must be released
only after liveness, the overlap sleep and a post-overlap PID recheck, and
must tear the helper down on every exit path (including terminate/kill
timeouts). One optional smoke test runs the real powershell.exe helper once,
without touching the tray EXE.
"""

from __future__ import annotations

import os
import queue
import shutil
import subprocess
import threading
from contextlib import contextmanager
from pathlib import Path

import pytest

import deploy_tray


class FakeClock:
    def __init__(self, step: float = 0.25) -> None:
        self._now = 0.0
        self._step = step

    def __call__(self) -> float:
        self._now += self._step
        return self._now


class _FakeGuardStdin:
    def __init__(self, events: list[object]) -> None:
        self.events = events
        self.closed = False

    def write(self, data: str) -> int:
        self.events.append("guard_stdin_write")
        return len(data)

    def flush(self) -> None:
        self.events.append("guard_stdin_flush")

    def close(self) -> None:
        if not self.closed:
            self.closed = True
            self.events.append("guard_stdin_closed")


class _FakeGuardStdout:
    """Delivers queued lines, then holds the reader until the process ends."""

    def __init__(
        self,
        lines: tuple[str, ...],
        on_sentinel: object | None = None,
        eof_event: threading.Event | None = None,
    ) -> None:
        self._lines = [f"{line}\n" for line in lines]
        self._on_sentinel = on_sentinel
        self._eof_event = eof_event or threading.Event()
        self.closed = False

    def readline(self) -> str:
        if self._lines:
            line = self._lines.pop(0)
            if self._on_sentinel is not None and line.strip() == deploy_tray.GUARD_SENTINEL:
                self._on_sentinel()
            return line
        # A live helper keeps stdout open; EOF arrives only at process end.
        self._eof_event.wait(timeout=0.5)
        return ""

    def close(self) -> None:
        if not self.closed:
            self.closed = True
            self._eof_event.set()


class _BlockingFakeStdout:
    """Stays silent so the handshake start timeout has to fire."""

    def __init__(self, timeout: float = 3.0) -> None:
        self._queue: queue.Queue[str] = queue.Queue()
        self._timeout = timeout
        self.closed = False

    def readline(self) -> str:
        try:
            return self._queue.get(timeout=self._timeout)
        except queue.Empty:
            return ""

    def close(self) -> None:
        self.closed = True


class _SyncedGuardStdout:
    """Keeps the reader pending after the sentinel until close() releases it.

    close() records ``stdout_closed_early`` when the reader was still blocked,
    so the drain-order test cannot pass without the real reader join.
    """

    def __init__(self, events: list[object], hold_seconds: float = 0.3) -> None:
        self._events = events
        self._hold_seconds = hold_seconds
        self._released = threading.Event()
        self._sentinel_sent = False
        self._reader_pending = False
        self.closed = False

    def readline(self) -> str:
        if not self._sentinel_sent:
            self._sentinel_sent = True
            return f"{deploy_tray.GUARD_SENTINEL}\n"
        self._reader_pending = True
        self._released.wait(timeout=self._hold_seconds)
        self._reader_pending = False
        self._events.append("drain_finished")
        return ""

    def close(self) -> None:
        if not self.closed:
            self.closed = True
            if self._reader_pending:
                self._events.append("stdout_closed_early")
            self._events.append("stdout_closed")
            self._released.set()


class FakeGuardProcess:
    """Popen stand-in for the powershell.exe guard helper."""

    def __init__(
        self,
        events: list[object],
        *,
        stdout_lines: tuple[str, ...] = (deploy_tray.GUARD_SENTINEL,),
        stdout: object | None = None,
        hang: str | None = None,
        die_after_sentinel: bool = False,
        death_code: int = 1,
    ) -> None:
        self.events = events
        self._eof_event = threading.Event()
        self.stdout = stdout if stdout is not None else _FakeGuardStdout(
            stdout_lines, on_sentinel=self._mark_sentinel, eof_event=self._eof_event
        )
        self.stdin = _FakeGuardStdin(events)
        self.hang = hang
        self.die_after_sentinel = die_after_sentinel
        self.death_code = death_code
        self.pid = 424242
        self._sentinel_delivered = False
        self._terminated = False
        self._killed = False
        self.returncode: int | None = None

    def _mark_sentinel(self) -> None:
        self._sentinel_delivered = True

    def poll(self) -> int | None:
        if self.die_after_sentinel and self._sentinel_delivered:
            if self.returncode is None:
                self.returncode = self.death_code
            return self.returncode
        return self.returncode

    def wait(self, timeout: float | None = None) -> int:
        hanging = (self.hang == "until_terminate" and not self._terminated) or (
            self.hang == "until_kill" and not self._killed
        )
        if hanging:
            self.events.append("guard_wait_timeout")
            raise subprocess.TimeoutExpired(cmd="powershell.exe", timeout=timeout)
        if self.returncode is None:
            self.returncode = -9 if self._killed else (-15 if self._terminated else 0)
        self._eof_event.set()
        self.events.append("guard_wait_ok")
        return self.returncode

    def terminate(self) -> None:
        self._terminated = True
        self.events.append("guard_terminate")
        self._eof_event.set()

    def kill(self) -> None:
        self._killed = True
        self.events.append("guard_kill")
        self._eof_event.set()


def make_guard_factory(events: list[object], proc: FakeGuardProcess):
    def factory():
        @contextmanager
        def _ctx():
            events.append("guard_start")
            with deploy_tray.hold_windows_power_guard(
                popen=lambda *args, **kwargs: proc, clock=FakeClock()
            ) as guard:
                events.append("guard_active")
                yield guard
            events.append("guard_released")

        return _ctx()

    return factory


def make_ps(
    events: list[object],
    *,
    stop_leftover: str = "0",
    start_ok: bool = True,
    liveness_pids: tuple[str, ...] = ("4242",),
):
    calls: list[str] = []
    live = list(liveness_pids)

    def fake_ps(command: str, *, timeout: float = 30.0) -> subprocess.CompletedProcess:
        calls.append(command)
        if "Stop-Process" in command:
            events.append("stop_old_tray")
            return subprocess.CompletedProcess(command, 0, stdout=stop_leftover)
        if "Start-Process" in command:
            events.append("start_new_tray")
            return subprocess.CompletedProcess(
                command,
                0 if start_ok else 1,
                stdout="",
                stderr=None if start_ok else "start boom",
            )
        if "Select-Object -First 1" in command:
            pid = live.pop(0) if live else ""
            events.append(f"liveness_pid:{pid}" if pid else "liveness_none")
            return subprocess.CompletedProcess(command, 0, stdout=pid)
        raise AssertionError(f"unexpected ps command: {command}")

    return fake_ps, calls


def make_copy2(events: list[object], fail_on: str | None = None):
    def fake_copy2(src: Path, dst: Path) -> None:
        events.append(f"copy:{src.name}->{dst.name}")
        if fail_on is not None and dst.name == fail_on:
            raise RuntimeError("simulated copy failure")
        dst.write_bytes(src.read_bytes() if src.exists() else b"x")

    return fake_copy2


def make_paths(tmp_path: Path) -> tuple[Path, Path, Path]:
    src = tmp_path / "herdr-night-watch.exe"
    src.write_bytes(b"NEWBUILD" * 4)
    dst = tmp_path / "Herdr-Nachtwaechter.exe"
    dst.write_bytes(b"OLD" * 4)
    prev = tmp_path / "Herdr-Nachtwaechter.prev.exe"
    return src, dst, prev


def run_main(events: list[object], proc: FakeGuardProcess, tmp_path: Path, **overrides):
    src, dst, prev = make_paths(tmp_path)
    ps_fn, _calls = make_ps(
        events,
        stop_leftover=overrides.pop("stop_leftover", "0"),
        start_ok=overrides.pop("start_ok", True),
        liveness_pids=overrides.pop("liveness_pids", ("4242",)),
    )
    kwargs = dict(
        ps_fn=ps_fn,
        copy2=make_copy2(events, fail_on=overrides.pop("fail_on", None)),
        sleep=lambda s: events.append(f"sleep:{s}"),
        clock=FakeClock(),
        guard_factory=make_guard_factory(events, proc),
        src=src,
        dst=dst,
        prev=prev,
        win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
    )
    kwargs.update(overrides)
    return deploy_tray.main(**kwargs)


def test_happy_path_activates_guard_before_stop_and_releases_after_overlap(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)
    sleeps: list[float] = []

    result = run_main(
        events,
        proc,
        tmp_path,
        liveness_pids=("4242", "4242"),
        sleep=lambda s: (sleeps.append(s), events.append(f"sleep:{s}")),
    )

    assert result == 0
    # 1. Reihenfolge-Invariante: Guard zuerst, Stop-Process nie vorher.
    assert events.index("guard_active") < events.index("stop_old_tray")
    # Backup und Kopieren liegen zwischen Stop und Start.
    backup_event = "copy:Herdr-Nachtwaechter.exe->Herdr-Nachtwaechter.prev.exe"
    copy_event = "copy:herdr-night-watch.exe->Herdr-Nachtwaechter.exe"
    assert events.index("stop_old_tray") < events.index(backup_event)
    assert events.index(backup_event) < events.index(copy_event)
    assert events.index(copy_event) < events.index("start_new_tray")
    # Liveness, dann Überlappungsschlaf, dann erst Freigabe (Kette gesichert).
    assert events.index("start_new_tray") < events.index("liveness_pid:4242")
    assert events.index("liveness_pid:4242") < events.index("sleep:2.0")
    assert events.index("sleep:2.0") < events.index("guard_stdin_write")
    assert sleeps == [deploy_tray.OVERLAP_AFTER_LIVE]
    # Post-Overlap-Recheck bestätigt die PID ein zweites Mal.
    assert events.count("liveness_pid:4242") == 2
    # Sauberes Teardown: abgeholt, kein Terminate/Kill im Happy Path.
    assert "guard_stdin_closed" in events
    assert "guard_wait_ok" in events
    assert "guard_terminate" not in events
    assert "guard_kill" not in events
    assert proc.returncode == 0
    out = capsys.readouterr().out
    assert "deployed pid=4242" in out
    assert "bytes=32" in out


def test_fail_closed_when_helper_dies_after_sentinel(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, die_after_sentinel=True)
    src, dst, prev = make_paths(tmp_path)
    ps_fn, ps_calls = make_ps(events)

    with pytest.raises(SystemExit, match="power guard lost before stop"):
        deploy_tray.main(
            ps_fn=ps_fn,
            copy2=make_copy2(events),
            sleep=lambda s: events.append(f"sleep:{s}"),
            clock=FakeClock(),
            guard_factory=make_guard_factory(events, proc),
            src=src,
            dst=dst,
            prev=prev,
            win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
        )

    # Der Guard-Verlust vor Stop-Process darf das Tray nie anfassen.
    assert ps_calls == []
    assert "stop_old_tray" not in events
    assert "start_new_tray" not in events
    # Der tote Helfer wurde trotzdem abgeräumt.
    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events
    assert proc.returncode == 1


def test_fail_closed_when_tray_exits_during_overlap(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)

    with pytest.raises(SystemExit, match="tray exited during overlap window"):
        run_main(events, proc, tmp_path, liveness_pids=("4242", ""))

    assert events.index("liveness_pid:4242") < events.index("sleep:2.0")
    assert events.index("sleep:2.0") < events.index("liveness_none")
    # Exception-Safety: Guard trotzdem sauber freigegeben.
    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events
    assert "guard_terminate" not in events


def test_liveness_polls_until_tray_process_appears(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)
    sleeps: list[float] = []
    src, dst, prev = make_paths(tmp_path)
    ps_fn, _calls = make_ps(events, liveness_pids=("", "777", "777"))

    result = deploy_tray.main(
        ps_fn=ps_fn,
        copy2=make_copy2(events),
        sleep=lambda s: (sleeps.append(s), events.append(f"sleep:{s}")),
        clock=FakeClock(step=0.25),
        guard_factory=make_guard_factory(events, proc),
        src=src,
        dst=dst,
        prev=prev,
        win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
    )

    assert result == 0
    assert events.index("liveness_none") < events.index("liveness_pid:777")
    assert sleeps == [deploy_tray.LIVENESS_POLL_INTERVAL, deploy_tray.OVERLAP_AFTER_LIVE]


def test_liveness_timeout_fails_after_releasing_guard(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)
    src, dst, prev = make_paths(tmp_path)
    ps_fn, _calls = make_ps(events, liveness_pids=())

    with pytest.raises(SystemExit, match="tray did not start"):
        deploy_tray.main(
            ps_fn=ps_fn,
            copy2=make_copy2(events),
            sleep=lambda s: events.append(f"sleep:{s}"),
            clock=FakeClock(step=0.25),
            guard_factory=make_guard_factory(events, proc),
            src=src,
            dst=dst,
            prev=prev,
            win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
        )

    # Exception-Safety: der Guard wurde trotzdem sauber freigegeben.
    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events
    assert "guard_terminate" not in events


@pytest.mark.parametrize(
    "stdout_lines,match",
    [
        ((), "failed before signalling"),
        (("PowerCreateRequest fehlgeschlagen",), "PowerCreateRequest fehlgeschlagen"),
    ],
)
def test_fail_closed_never_stops_tray_without_sentinel(
    tmp_path: Path, stdout_lines: tuple[str, ...], match: str
) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, stdout_lines=stdout_lines)
    src, dst, prev = make_paths(tmp_path)
    ps_fn, ps_calls = make_ps(events)

    with pytest.raises(SystemExit, match=match):
        deploy_tray.main(
            ps_fn=ps_fn,
            copy2=make_copy2(events),
            sleep=lambda s: events.append(f"sleep:{s}"),
            clock=FakeClock(),
            guard_factory=make_guard_factory(events, proc),
            src=src,
            dst=dst,
            prev=prev,
            win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
        )

    # Fail-Closed: kein einziger PowerShell-Aufruf, Stop-Process lief nie.
    assert ps_calls == []
    assert "stop_old_tray" not in events
    assert "start_new_tray" not in events
    # Der gescheiterte Helfer wurde trotzdem abgeräumt (kein Waisenprozess).
    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events


def test_fail_closed_on_handshake_start_timeout() -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, stdout=_BlockingFakeStdout())
    guard = deploy_tray.WindowsPowerGuard(
        popen=lambda *args, **kwargs: proc,
        clock=FakeClock(step=0.05),
        start_timeout=0.2,
    )

    with pytest.raises(SystemExit, match="did not signal"):
        guard.start()

    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events


def test_copy_failure_still_releases_guard(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)

    with pytest.raises(RuntimeError, match="simulated copy failure"):
        run_main(events, proc, tmp_path, fail_on="Herdr-Nachtwaechter.exe")

    backup_event = "copy:Herdr-Nachtwaechter.exe->Herdr-Nachtwaechter.prev.exe"
    assert events.index("stop_old_tray") < events.index(backup_event)
    assert "guard_stdin_write" in events
    assert "guard_stdin_closed" in events
    assert "guard_wait_ok" in events
    assert "guard_terminate" not in events
    assert proc.returncode == 0


def test_start_failure_still_releases_guard(tmp_path: Path) -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events)
    src, dst, prev = make_paths(tmp_path)
    ps_fn, _calls = make_ps(events, start_ok=False)

    with pytest.raises(SystemExit, match="start failed"):
        deploy_tray.main(
            ps_fn=ps_fn,
            copy2=make_copy2(events),
            sleep=lambda s: events.append(f"sleep:{s}"),
            clock=FakeClock(),
            guard_factory=make_guard_factory(events, proc),
            src=src,
            dst=dst,
            prev=prev,
            win_dst=r"C:\fake\Herdr-Nachtwaechter.exe",
        )

    assert events.index("copy:herdr-night-watch.exe->Herdr-Nachtwaechter.exe") < events.index(
        "start_new_tray"
    )
    assert "guard_stdin_write" in events
    assert "guard_wait_ok" in events
    assert "guard_kill" not in events


def test_teardown_terminates_hanging_helper_and_is_idempotent() -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, hang="until_terminate")
    guard = deploy_tray.WindowsPowerGuard(
        popen=lambda *args, **kwargs: proc,
        clock=FakeClock(),
        release_timeout=0.05,
        terminate_timeout=0.05,
    )
    guard.start()
    guard.close()

    assert events.index("guard_wait_timeout") < events.index("guard_terminate")
    assert events.index("guard_terminate") < events.index("guard_wait_ok")
    assert "guard_kill" not in events
    assert guard.exit_code == -15

    # Idempotent: ein zweites close() startet kein erneutes Teardown.
    guard.close()
    assert events.count("guard_terminate") == 1
    assert events.count("guard_wait_ok") == 1


def test_teardown_kills_helper_that_ignores_terminate() -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, hang="until_kill")
    guard = deploy_tray.WindowsPowerGuard(
        popen=lambda *args, **kwargs: proc,
        clock=FakeClock(),
        release_timeout=0.05,
        terminate_timeout=0.05,
    )
    guard.start()
    guard.close()

    assert events.index("guard_terminate") < events.index("guard_kill")
    assert events.index("guard_kill") < events.index("guard_wait_ok")
    assert guard.exit_code == -9


def test_close_joins_drain_thread_before_closing_stdout() -> None:
    events: list[object] = []
    proc = FakeGuardProcess(events, stdout=_SyncedGuardStdout(events))
    guard = deploy_tray.WindowsPowerGuard(
        popen=lambda *args, **kwargs: proc, clock=FakeClock()
    )
    guard.start()
    guard.close()

    # Der Reader wurde vor dem stdout.close() abgeholt; ohne join wuerde der
    # synchronisierte Fake "stdout_closed_early" aufzeichnen und den Test
    # fehlschlagen lassen.
    assert "drain_finished" in events
    assert "stdout_closed_early" not in events
    assert events.index("drain_finished") < events.index("stdout_closed")


def test_helper_command_wires_windows_power_apis() -> None:
    events: list[object] = []
    captured: dict[str, object] = {}

    def fake_popen(argv, **kwargs):
        captured["argv"] = argv
        captured["kwargs"] = kwargs
        return FakeGuardProcess(events)

    guard = deploy_tray.WindowsPowerGuard(popen=fake_popen, clock=FakeClock())
    guard.start()
    guard.close()

    argv = captured["argv"]
    assert argv[0] == "powershell.exe"
    assert argv[1] == "-NoProfile"
    script = argv[3]
    assert "PowerCreateRequest" in script
    assert "SetThreadExecutionState" in script
    assert "2147483648" in script  # ES_CONTINUOUS als dezimales UInt32
    assert "$ES_SYSTEM_REQUIRED" in script
    assert deploy_tray.GUARD_SENTINEL in script
    # S1-1: PowerRequestSystemRequired ist Enum-Wert 1 (0 wäre DisplayRequired).
    assert "$POWER_REQUEST_SYSTEM_REQUIRED = [int]1" in script
    assert "PowerSetRequest($request, $POWER_REQUEST_SYSTEM_REQUIRED)" in script
    assert "PowerClearRequest($request, $POWER_REQUEST_SYSTEM_REQUIRED)" in script
    # S2-1: INVALID_HANDLE_VALUE (-1) wird genauso wie NULL abgewiesen.
    assert "[IntPtr](-1)" in script
    assert "Size = 24" in script  # native Union-Groesse im 64-Bit-ABI
    kwargs = captured["kwargs"]
    assert kwargs["stdin"] == subprocess.PIPE
    assert kwargs["stdout"] == subprocess.PIPE
    assert kwargs["stderr"] == subprocess.STDOUT
    assert kwargs["text"] is True
    assert kwargs["errors"] == "replace"


def test_helper_script_releases_power_request_in_finally() -> None:
    events: list[object] = []
    captured: dict[str, object] = {}

    def fake_popen(argv, **kwargs):
        captured["argv"] = argv
        return FakeGuardProcess(events)

    guard = deploy_tray.WindowsPowerGuard(popen=fake_popen, clock=FakeClock())
    guard.start()
    guard.close()
    script = captured["argv"][3]

    # S2-2: Haltephase im try, kompletter Cleanup inkl. FreeHGlobal im finally.
    try_pos = script.index("try {")
    hold_pos = script.index("[void][Console]::In.ReadLine()")
    finally_pos = script.index("finally {")
    assert try_pos < hold_pos < finally_pos
    assert script.index("}", 0, finally_pos) < finally_pos  # try-Block schließt vor finally
    for cleanup_call in (
        "PowerClearRequest($request, $POWER_REQUEST_SYSTEM_REQUIRED)",
        "CloseHandle($request)",
        "SetThreadExecutionState($ES_CONTINUOUS)",
        "FreeHGlobal($reasonPtr)",
    ):
        assert finally_pos < script.index(cleanup_call)


def test_ps_reads_windows_output_as_utf8_with_replacement() -> None:
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):
        captured["argv"] = argv
        captured["kwargs"] = kwargs
        return subprocess.CompletedProcess(argv, 0, stdout="0")

    result = deploy_tray.ps("Get-Thing", run=fake_run)

    assert result.stdout == "0"
    assert captured["argv"][0] == "powershell.exe"
    assert captured["kwargs"]["encoding"] == "utf-8"
    assert captured["kwargs"]["errors"] == "replace"
    assert captured["kwargs"]["stdin"] == subprocess.DEVNULL


def test_wait_for_tray_alive_bounds_each_probe_by_remaining_budget() -> None:
    events: list[object] = []
    timeouts: list[float] = []
    ps_fn, _calls = make_ps(events, liveness_pids=("777",))

    def probing_ps(command: str, *, timeout: float = 30.0) -> subprocess.CompletedProcess:
        timeouts.append(timeout)
        return ps_fn(command, timeout=timeout)

    pid = deploy_tray.wait_for_tray_alive(
        ps_fn=probing_ps,
        sleep=lambda s: events.append(f"sleep:{s}"),
        clock=FakeClock(step=4.0),
        liveness_timeout=10.0,
    )

    assert pid == "777"
    # Erste Abfrage startet nach einem Clock-Tick (4.0 verstrichen): Restbudget 6.0.
    assert timeouts == [min(30.0, 10.0 - 4.0)]


def test_real_windows_power_guard_smoke() -> None:
    """One real helper run: sentinel must arrive and the helper must exit 0.

    Only touches the power APIs, never the Herdr-Nachtwaechter.exe tray.
    """
    if os.environ.get("HERDR_SKIP_LIVE_GUARD"):
        pytest.skip("HERDR_SKIP_LIVE_GUARD gesetzt")
    if shutil.which("powershell.exe") is None:
        pytest.skip("powershell.exe (WSL-Interop) nicht verfuegbar")

    guard = deploy_tray.WindowsPowerGuard(start_timeout=25.0, release_timeout=15.0)
    guard.start()
    guard.ensure_active(stage="smoke test")
    guard.close()

    assert guard.exit_code == 0
