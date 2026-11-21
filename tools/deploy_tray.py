#!/usr/bin/env python3
"""Replace the installed tray EXE and restart only the tray process.

Stopping the old tray EXE drops its PowerRequestSystemRequired request.
When the interactive user has been idle longer than the standby idle
timeout (STANDBYIDLE, 3 h), Windows can send the PC to standby within
seconds and locks the session on resume. The deploy therefore holds an
overlapping Windows power guard in a short-lived powershell.exe helper:
it activates PowerCreateRequest/PowerSetRequest(PowerRequestSystemRequired)
plus SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) (mirroring
src/power_guard.rs, no admin rights required) and only reports
POWER_GUARD_ACTIVE once both hold. The old EXE is never stopped before
that sentinel arrives (fail-closed), and the guard is released only after
the new EXE passed the liveness check plus an overlap window that gives
its own tray power guard time to come up.
"""

from __future__ import annotations

import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from collections.abc import Callable, Iterator
from contextlib import AbstractContextManager, contextmanager
from pathlib import Path

SRC = Path("/home/simon/projects/herdr-night-watch/target/x86_64-pc-windows-gnu/release/herdr-night-watch.exe")
DST = Path("/mnt/c/Users/Simon/Apps/HerdrNachtwaechter/Herdr-Nachtwaechter.exe")
PREV = Path("/mnt/c/Users/Simon/Apps/HerdrNachtwaechter/Herdr-Nachtwaechter.prev.exe")
WIN_DST = r"C:\Users\Simon\Apps\HerdrNachtwaechter\Herdr-Nachtwaechter.exe"

GUARD_SENTINEL = "POWER_GUARD_ACTIVE"
GUARD_START_TIMEOUT = 10.0
GUARD_RELEASE_TIMEOUT = 8.0
GUARD_TERMINATE_TIMEOUT = 2.0
LIVENESS_TIMEOUT = 10.0
LIVENESS_POLL_INTERVAL = 0.5
OVERLAP_AFTER_LIVE = 2.0

# P/Invoke helper: holds PowerRequestSystemRequired + ES_CONTINUOUS |
# ES_SYSTEM_REQUIRED until one line arrives on stdin, then clears both
# and exits 0. Any activation failure exits 1 without the sentinel.
_PS_GUARD_SCRIPT = r"""
$ErrorActionPreference = 'Stop'
$source = @'
using System;
using System.Runtime.InteropServices;

public static class DeployPowerGuard
{
    [StructLayout(LayoutKind.Explicit, Size = 24)]
    public struct REASON_CONTEXT_UNION
    {
        [FieldOffset(0)] public IntPtr SimpleReasonString;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct REASON_CONTEXT
    {
        public uint Version;
        public uint Flags;
        public REASON_CONTEXT_UNION Reason;
    }

    [DllImport("kernel32.dll")]
    public static extern IntPtr PowerCreateRequest(ref REASON_CONTEXT context);

    [DllImport("kernel32.dll")]
    public static extern bool PowerSetRequest(IntPtr request, int requestType);

    [DllImport("kernel32.dll")]
    public static extern bool PowerClearRequest(IntPtr request, int requestType);

    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll")]
    public static extern uint SetThreadExecutionState(uint flags);
}
'@
Add-Type -TypeDefinition $source

# PowerShell parses 0x80000000-style literals as negative Int32, which the
# UInt32 parameter binding rejects; decimal UInt32 values avoid that trap.
$ES_CONTINUOUS = [uint32]2147483648
$ES_SYSTEM_REQUIRED = [uint32]1
# POWER_REQUEST_TYPE: PowerRequestDisplayRequired = 0, PowerRequestSystemRequired = 1.
$POWER_REQUEST_SYSTEM_REQUIRED = [int]1

$reasonPtr = [System.Runtime.InteropServices.Marshal]::StringToHGlobalUni('Herdr Nachtwaechter Deploy-Uebergang')
$reason = New-Object DeployPowerGuard+REASON_CONTEXT_UNION
$reason.SimpleReasonString = $reasonPtr
$context = New-Object DeployPowerGuard+REASON_CONTEXT
$context.Version = [uint32]0
$context.Flags = [uint32]1
$context.Reason = $reason

# PowerCreateRequest reports failure as INVALID_HANDLE_VALUE (-1) or NULL.
$request = [IntPtr]::Zero
try {
    $request = [DeployPowerGuard]::PowerCreateRequest([ref]$context)
    if ($request -eq [IntPtr]::Zero -or $request -eq [IntPtr](-1)) {
        [Console]::Error.WriteLine('PowerCreateRequest fehlgeschlagen')
        exit 1
    }
    if (-not [DeployPowerGuard]::PowerSetRequest($request, $POWER_REQUEST_SYSTEM_REQUIRED)) {
        [Console]::Error.WriteLine('PowerSetRequest fehlgeschlagen')
        exit 1
    }
    if ([DeployPowerGuard]::SetThreadExecutionState([uint32]($ES_CONTINUOUS -bor $ES_SYSTEM_REQUIRED)) -eq [uint32]0) {
        [Console]::Error.WriteLine('SetThreadExecutionState fehlgeschlagen')
        exit 1
    }

    [Console]::Out.WriteLine('POWER_GUARD_ACTIVE')
    [Console]::Out.Flush()

    # Hold the power request until deploy_tray releases us via one line.
    [void][Console]::In.ReadLine()
}
finally {
    if ($request -ne [IntPtr]::Zero -and $request -ne [IntPtr](-1)) {
        [void][DeployPowerGuard]::PowerClearRequest($request, $POWER_REQUEST_SYSTEM_REQUIRED)
        [void][DeployPowerGuard]::CloseHandle($request)
    }
    [void][DeployPowerGuard]::SetThreadExecutionState($ES_CONTINUOUS)
    [System.Runtime.InteropServices.Marshal]::FreeHGlobal($reasonPtr)
}
exit 0
"""


class WindowsPowerGuard:
    """Overlapping Windows standby guard held by a powershell.exe helper.

    start() launches the helper, drains its stdout on a reader thread and
    returns only after GUARD_SENTINEL arrived. Every other outcome (EOF,
    helper error output, start timeout) fails closed via SystemExit after
    the helper was torn down, so callers never touch the running tray EXE
    without an active power request.
    """

    def __init__(
        self,
        *,
        popen: Callable[..., subprocess.Popen] = subprocess.Popen,
        clock: Callable[[], float] = time.monotonic,
        start_timeout: float = GUARD_START_TIMEOUT,
        release_timeout: float = GUARD_RELEASE_TIMEOUT,
        terminate_timeout: float = GUARD_TERMINATE_TIMEOUT,
    ) -> None:
        self._popen = popen
        self._clock = clock
        self._start_timeout = start_timeout
        self._release_timeout = release_timeout
        self._terminate_timeout = terminate_timeout
        self._proc: subprocess.Popen | None = None
        self._reader: threading.Thread | None = None
        self._lines: queue.Queue[str | None] | None = None
        self._helper_gone = False
        self.exit_code: int | None = None

    def start(self) -> None:
        if self._proc is not None:
            raise RuntimeError("power guard already started")
        try:
            proc = self._popen(
                ["powershell.exe", "-NoProfile", "-Command", _PS_GUARD_SCRIPT],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
        except OSError as exc:
            raise SystemExit(f"power guard failed to start: {exc}") from exc
        self._proc = proc
        lines: queue.Queue[str | None] = queue.Queue()
        self._lines = lines

        def _drain() -> None:
            stdout = proc.stdout
            try:
                if stdout is None:
                    return
                while True:
                    try:
                        line = stdout.readline()
                    except (OSError, ValueError):
                        break
                    if not line:
                        break
                    lines.put(line.strip())
            finally:
                # EOF means the helper can no longer hold the power request.
                self._helper_gone = True
                lines.put(None)

        reader = threading.Thread(target=_drain, daemon=True)
        self._reader = reader
        reader.start()
        collected: list[str] = []
        deadline = self._clock() + self._start_timeout
        while True:
            remaining = deadline - self._clock()
            if remaining <= 0:
                self.close()
                raise SystemExit(
                    f"power guard did not signal {GUARD_SENTINEL} within "
                    f"{self._start_timeout}s: {collected}"
                )
            try:
                item = lines.get(timeout=remaining)
            except queue.Empty:
                continue
            if item is None:
                self.close()
                detail = f": {collected}" if collected else ""
                raise SystemExit(
                    f"power guard failed before signalling {GUARD_SENTINEL}{detail}"
                )
            if item == GUARD_SENTINEL:
                return
            collected.append(item)

    def ensure_active(self, *, stage: str) -> None:
        """Fail closed when the helper ended after signalling the sentinel.

        The sentinel only proves the requests were active once; a helper that
        died since (crash, external kill, early stdin EOF) leaves the handover
        without power protection. Callers must check this before and during
        the phase where the old tray EXE is stopped and the new one starts.
        """
        proc = self._proc
        if proc is None:
            raise SystemExit(f"power guard inactive at {stage}: helper not running")
        exit_code = proc.poll()
        if self._helper_gone or exit_code is not None:
            self.close()
            detail = f"exit code {exit_code}" if exit_code is not None else "stdout closed early"
            raise SystemExit(f"power guard lost before {stage}: helper ended ({detail})")

    def close(self) -> None:
        proc = self._proc
        if proc is None:
            return
        self._proc = None
        try:
            stdin = getattr(proc, "stdin", None)
            if stdin is not None and not stdin.closed:
                try:
                    stdin.write("\n")
                    stdin.flush()
                except (OSError, ValueError):
                    pass
                try:
                    stdin.close()
                except (OSError, ValueError):
                    pass
        finally:
            self._join_helper(proc)

    def _join_helper(self, proc: subprocess.Popen) -> None:
        try:
            proc.wait(timeout=self._release_timeout)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=self._terminate_timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        if proc.returncode is not None:
            self.exit_code = proc.returncode
            if self.exit_code != 0:
                print(
                    f"warning: power guard helper exited with code {self.exit_code}",
                    file=sys.stderr,
                )
        # Join the drain thread before closing its stdout underneath it.
        reader = self._reader
        if reader is not None and reader.is_alive():
            reader.join(timeout=1.0)
        stdout = getattr(proc, "stdout", None)
        if stdout is not None and not stdout.closed:
            stdout.close()


@contextmanager
def hold_windows_power_guard(**kwargs: object) -> Iterator[WindowsPowerGuard]:
    """Hold the overlapping Windows power guard for a deploy handover."""
    guard = WindowsPowerGuard(**kwargs)  # type: ignore[arg-type]
    try:
        # start() inside try: an interruption during the handshake must still
        # tear the helper down instead of leaving it with active requests.
        guard.start()
        yield guard
    finally:
        guard.close()


def ps(
    command: str,
    *,
    run: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    timeout: float = 30.0,
) -> subprocess.CompletedProcess[str]:
    return run(
        ["powershell.exe", "-NoProfile", "-Command", command],
        check=False,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        stdin=subprocess.DEVNULL,
        timeout=timeout,
    )


def wait_for_tray_alive(
    *,
    ps_fn: Callable[..., subprocess.CompletedProcess[str]],
    sleep: Callable[[float], None],
    clock: Callable[[], float],
    liveness_timeout: float = LIVENESS_TIMEOUT,
    poll_interval: float = LIVENESS_POLL_INTERVAL,
) -> str:
    """Poll until at least one tray process is running again, then return its PID.

    Each probe is bounded by the remaining liveness budget so a hanging
    powershell probe cannot extend the wait far beyond ``liveness_timeout``.
    The deadline is re-checked before every probe and each polling sleep is
    capped at the remaining budget, so no new probe starts once the budget
    is exhausted; a probe that started in time may still return a late PID.
    """
    deadline = clock() + liveness_timeout
    while True:
        remaining = deadline - clock()
        if remaining <= 0:
            raise SystemExit("tray did not start")
        check = ps_fn(LIVENESS_CMD, timeout=min(30.0, max(remaining, 0.1)))
        pid = (check.stdout or "").strip()
        if pid:
            return pid
        sleep(min(poll_interval, max(deadline - clock(), 0.0)))


RECOVERY_LIVENESS_TIMEOUT = 5.0
LIVENESS_CMD = (
    "Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | "
    "Select-Object -First 1 -ExpandProperty Id"
)


def staged_replace(src: Path, dst: Path, copy2: Callable[[Path, Path], object]) -> Path:
    """Copy to a temp file next to dst, then swap dst in atomically.

    os.replace guarantees that dst is either the complete old file or the
    complete new build, never a half-written executable. The staging name
    carries the deploy process id so overlapping deploys for the same
    destination cannot rename each other's staging file.
    """
    tmp = dst.with_name(f"{dst.name}.{os.getpid()}.deploying.tmp")
    copy2(src, tmp)
    os.replace(tmp, dst)
    return tmp


def tray_is_running(ps_fn: Callable[..., subprocess.CompletedProcess[str]]) -> bool:
    check = ps_fn(LIVENESS_CMD, timeout=30.0)
    return bool((check.stdout or "").strip())


def _start_and_confirm(
    *,
    ps_fn: Callable[..., subprocess.CompletedProcess[str]],
    sleep: Callable[[float], None],
    clock: Callable[[], float],
    win_dst: str,
    liveness_timeout: float,
) -> None:
    start = ps_fn(f"Start-Process -FilePath '{win_dst}'", timeout=30.0)
    if start.returncode != 0:
        raise SystemExit(f"recovery start failed: {start.stderr or start.stdout}")
    try:
        wait_for_tray_alive(
            ps_fn=ps_fn, sleep=sleep, clock=clock, liveness_timeout=liveness_timeout
        )
    except SystemExit as exc:
        raise SystemExit(f"recovery failed: {exc}") from exc


def recover_tray_after_failure(
    *,
    ps_fn: Callable[..., subprocess.CompletedProcess[str]],
    copy2: Callable[[Path, Path], object],
    sleep: Callable[[float], None],
    clock: Callable[[], float],
    guard_factory: Callable[[], AbstractContextManager[WindowsPowerGuard]],
    dst: Path,
    prev: Path,
    win_dst: str,
) -> None:
    """Best-effort tray recovery under its own freshly acquired guard.

    Runs whenever the deploy failed after the old tray was stopped. The
    original guard may already be gone (helper death), so recovery first
    acquires a new power guard and only then touches files or processes.
    It tries the current executable first; if that cannot run, the backup
    is restored and started. Every guard is released by its with-block, so
    no helper process is orphaned even when recovery itself fails.
    """
    with guard_factory() as recovery_guard:
        recovery_guard.ensure_active(stage="recovery")

        def restore_backup(reason: str) -> None:
            if not prev.is_file():
                raise SystemExit(f"recovery failed: {reason} and no backup available")
            copy2(prev, dst)

        if not dst.is_file() or dst.stat().st_size == 0:
            restore_backup("tray exe missing or empty")
        try:
            _start_and_confirm(
                ps_fn=ps_fn, sleep=sleep, clock=clock, win_dst=win_dst,
                liveness_timeout=RECOVERY_LIVENESS_TIMEOUT,
            )
        except (SystemExit, OSError, subprocess.TimeoutExpired):
            # The current build (often the just-installed replacement)
            # cannot stay running: fall back to the previous build.
            restore_backup("replacement did not stay running")
            _start_and_confirm(
                ps_fn=ps_fn, sleep=sleep, clock=clock, win_dst=win_dst,
                liveness_timeout=RECOVERY_LIVENESS_TIMEOUT,
            )
        recovery_guard.ensure_active(stage="recovery-confirmed")


def main(
    *,
    ps_fn: Callable[..., subprocess.CompletedProcess[str]] | None = None,
    copy2: Callable[[Path, Path], object] = shutil.copy2,
    sleep: Callable[[float], None] = time.sleep,
    clock: Callable[[], float] = time.monotonic,
    guard_factory: Callable[[], AbstractContextManager[WindowsPowerGuard]] = hold_windows_power_guard,
    src: Path = SRC,
    dst: Path = DST,
    prev: Path = PREV,
    win_dst: str = WIN_DST,
) -> int:
    if ps_fn is None:
        ps_fn = ps
    if not src.is_file():
        raise SystemExit(f"missing build: {src}")
    with guard_factory() as guard:
        guard.ensure_active(stage="stop")
        try:
            # The stop command itself may time out after the tray already
            # died, so it runs inside the guarded block and every failure
            # from here on goes through recovery.
            stop = ps_fn(
                "Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | Stop-Process -Force; "
                "Start-Sleep -Milliseconds 400; "
                "(Get-Process -Name 'Herdr-Nachtwaechter' -ErrorAction SilentlyContinue | Measure-Object).Count",
                timeout=30.0,
            )
            if stop.returncode != 0:
                raise SystemExit(f"stop failed: {stop.stderr or stop.stdout}")
            leftover = (stop.stdout or "").strip()
            if leftover not in {"", "0"}:
                raise SystemExit(f"tray still running after stop: {leftover}")
            guard.ensure_active(stage="replace")
            if dst.exists():
                copy2(dst, prev)
            staged_replace(src, dst, copy2)
            start = ps_fn(f"Start-Process -FilePath '{win_dst}'", timeout=30.0)
            if start.returncode != 0:
                raise SystemExit(f"start failed: {start.stderr or start.stdout}")
            guard.ensure_active(stage="handover")
            pid = wait_for_tray_alive(ps_fn=ps_fn, sleep=sleep, clock=clock)
            guard.ensure_active(stage="liveness")
            sleep(OVERLAP_AFTER_LIVE)
            # The replacement must still run after the overlap window while
            # the guard is held; report failure instead of a dead PID.
            confirm = ps_fn(LIVENESS_CMD, timeout=30.0)
            if not (confirm.stdout or "").strip():
                raise SystemExit("tray exited during overlap window")
            guard.ensure_active(stage="overlap")
        except BaseException as exc:
            # Every failure from the stop command on leaves the machine
            # potentially without a tray; restart one under a fresh guard,
            # then surface the original deploy error (a failing recovery
            # must never replace it). Both guards are released by their
            # with-blocks, so no helper process is orphaned.
            try:
                if tray_is_running(ps_fn):
                    print(
                        f"deploy failed ({exc}); tray still running, no recovery needed",
                        file=sys.stderr,
                    )
                else:
                    recover_tray_after_failure(
                        ps_fn=ps_fn, copy2=copy2, sleep=sleep, clock=clock,
                        guard_factory=guard_factory, dst=dst, prev=prev, win_dst=win_dst,
                    )
                    print(f"deploy failed ({exc}); tray was recovered", file=sys.stderr)
            except BaseException as recovery_exc:
                print(
                    f"deploy failed ({exc}) and tray recovery failed: {recovery_exc}",
                    file=sys.stderr,
                )
            raise
    print(f"deployed pid={pid} bytes={dst.stat().st_size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
