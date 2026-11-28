"""Fokussierte Unit- und Integrations-Tests für die Retention-/Ringpuffer-Deckel in tools/llm_account.py."""

# ruff: noqa: F811  (pytest-Fixture wird aus test_llm_account importiert)

from __future__ import annotations

import errno
import fcntl
import json
import os
import stat
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import llm_account as la  # noqa: E402
from test_llm_account import env as env_fixture  # noqa: E402, F401
from test_llm_account import tsv, write_account  # noqa: E402


def _make_cli_logs(log_dir: Path, count: int, *, base_epoch: float, step_s: float = 1.0) -> list[str]:
    """Erzeugt `count` zeitgestempelte cli-YYYYMMDD_HHMMSS.log-Dateien mit aufsteigender mtime."""
    log_dir.mkdir(parents=True, exist_ok=True)
    names: list[str] = []
    for idx in range(count):
        ts = base_epoch + idx * step_s
        stamp = time.strftime("%Y%m%d_%H%M%S", time.gmtime(ts))
        name = f"cli-{stamp}.log"
        path = log_dir / name
        path.write_text(f"log-entry-{idx}\n", encoding="utf-8")
        os.utime(path, (ts, ts))
        names.append(name)
    return names


def test_profile_log_ring_buffer_keeps_100_newest_and_preserves_protected_files(tmp_path):
    state = tmp_path / "state"
    accounts_json = state / "accounts.json"
    state.mkdir(parents=True)
    accounts_json.write_text('{"version": 1, "providers": {}}\n', encoding="utf-8")

    pdir = state / "profiles" / "agy" / "acc1"
    write_account(pdir, "a@example.test")
    agy_root = pdir / ".gemini" / "antigravity-cli"
    brain_file = agy_root / "brain" / "conv-1" / "notes.md"
    brain_file.parent.mkdir(parents=True)
    brain_file.write_text("brain-data", encoding="utf-8")
    conv_file = agy_root / "conversations" / "conv-1.pb"
    conv_file.parent.mkdir(parents=True)
    conv_file.write_bytes(b"conv-data")

    log_dir = agy_root / "log"
    now = 1_800_000_000.0
    created = _make_cli_logs(log_dir, 150, base_epoch=now - 1500, step_s=10.0)

    # Aktiver ../cli.log-Symlink (wie live von agy angelegt) sowie weitere geschützte Dateien/Symlinks in log/
    cli_symlink = agy_root / "cli.log"
    os.symlink(f"log/{created[-1]}", cli_symlink)

    cli_plain_in_log = log_dir / "cli.log"
    cli_plain_in_log.write_text("active-plain-cli-log\n", encoding="utf-8")

    foreign_file = log_dir / "custom-debug.log"
    foreign_file.write_text("foreign-keep\n", encoding="utf-8")

    external_target = tmp_path / "outside-secret.txt"
    external_target.write_text("DO-NOT-DELETE", encoding="utf-8")
    log_symlink = log_dir / "cli-20200101_000001.log"
    os.symlink(str(external_target), log_symlink)

    nested_dir = log_dir / "cli-20200101_000002.log"
    nested_dir.mkdir()
    nested_child = nested_dir / "inside.txt"
    nested_child.write_text("dir-keep", encoding="utf-8")

    res = la.prune_profile_logs(pdir, now=now)

    assert res["checked"] is True
    assert res["deleted"] == 50
    assert res["remaining_excess"] == 0
    assert res["skipped_cooldown"] is False
    assert res["error"] is None

    remaining_cli_logs = sorted(
        e.name
        for e in os.scandir(log_dir)
        if la.AGY_PROFILE_LOG_NAME_RE.fullmatch(e.name) and e.is_file(follow_symlinks=False)
    )
    assert remaining_cli_logs == sorted(created[-100:])

    # Alle geschützten Dateien, Verzeichnisse und Symlinks sind unangetastet.
    assert (pdir / la.AgyAdapter.token_rel).is_file()
    assert (pdir / la.AgyAdapter.scope_rel).is_file()
    assert brain_file.read_text(encoding="utf-8") == "brain-data"
    assert conv_file.read_bytes() == b"conv-data"
    assert accounts_json.exists()
    assert cli_symlink.is_symlink() and os.readlink(cli_symlink) == f"log/{created[-1]}"
    assert cli_plain_in_log.read_text(encoding="utf-8") == "active-plain-cli-log\n"
    assert foreign_file.read_text(encoding="utf-8") == "foreign-keep\n"
    assert log_symlink.is_symlink()
    assert external_target.read_text(encoding="utf-8") == "DO-NOT-DELETE"
    assert nested_dir.is_dir() and nested_child.read_text(encoding="utf-8") == "dir-keep"


def test_profile_log_symlinked_log_dir_is_never_followed(tmp_path):
    outside = tmp_path / "outside_logs"
    now = 1_800_000_000.0
    created = _make_cli_logs(outside, 120, base_epoch=now - 1200, step_s=10.0)

    pdir = tmp_path / "profile"
    agy_root = pdir / ".gemini" / "antigravity-cli"
    agy_root.mkdir(parents=True)
    os.symlink(str(outside), agy_root / "log")

    res = la.prune_profile_logs(pdir, now=now)

    assert res["checked"] is False
    assert res["deleted"] == 0
    assert len(list(outside.iterdir())) == len(created)


def test_profile_log_batch_limit_and_cooldown_drain_backlog_incrementally(tmp_path):
    pdir = tmp_path / "profile"
    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = 1_800_000_000.0
    created = _make_cli_logs(log_dir, 150, base_epoch=now - 1500, step_s=10.0)

    # Lauf 1: genau 20 älteste Dateien abbauen
    r1 = la.prune_profile_logs(pdir, max_files=100, batch_limit=20, cooldown_s=600.0, now=now)
    assert r1 == {
        "checked": True,
        "deleted": 20,
        "remaining_excess": 30,
        "skipped_cooldown": False,
        "unlink_errors": 0,
        "error": None,
    }
    assert sorted(p.name for p in log_dir.iterdir()) == sorted(created[20:])

    # Lauf 2 innerhalb des Cooldowns (t + 300s): sofort übersprungen
    r2 = la.prune_profile_logs(pdir, max_files=100, batch_limit=20, cooldown_s=600.0, now=now + 300.0)
    assert r2 == {
        "checked": False,
        "deleted": 0,
        "remaining_excess": 0,
        "skipped_cooldown": True,
        "unlink_errors": 0,
        "error": None,
    }
    assert len(list(log_dir.iterdir())) == 130

    # Lauf 3 nach Ablauf des Cooldowns (t + 600s): nächste 20 älteste Dateien
    r3 = la.prune_profile_logs(pdir, max_files=100, batch_limit=20, cooldown_s=600.0, now=now + 600.0)
    assert r3 == {
        "checked": True,
        "deleted": 20,
        "remaining_excess": 10,
        "skipped_cooldown": False,
        "unlink_errors": 0,
        "error": None,
    }
    assert sorted(p.name for p in log_dir.iterdir()) == sorted(created[40:])

    # Lauf 4 nach weiterem Cooldown (t + 1200s): restliche 10 -> genau 100 neueste verbleiben
    r4 = la.prune_profile_logs(pdir, max_files=100, batch_limit=20, cooldown_s=600.0, now=now + 1200.0)
    assert r4 == {
        "checked": True,
        "deleted": 10,
        "remaining_excess": 0,
        "skipped_cooldown": False,
        "unlink_errors": 0,
        "error": None,
    }
    assert sorted(p.name for p in log_dir.iterdir()) == sorted(created[50:])


def test_profile_log_single_unlink_error_does_not_abort_batch_and_sets_stamp(tmp_path, monkeypatch):
    pdir = tmp_path / "profile"
    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = 1_800_000_000.0
    created = _make_cli_logs(log_dir, 120, base_epoch=now - 1200, step_s=10.0)
    blocked_name = created[0]

    real_unlink = os.unlink

    def flaky_unlink(path, *, dir_fd=None):
        if path == blocked_name:
            raise PermissionError(errno.EACCES, "Permission denied", str(path))
        return real_unlink(path, dir_fd=dir_fd)

    monkeypatch.setattr(os, "unlink", flaky_unlink)

    res = la.prune_profile_logs(pdir, max_files=100, batch_limit=1000, cooldown_s=600.0, now=now)

    # (a) Alle übrigen 19 fälligen Dateien wurden trotz des ersten Fehlers abgebaut.
    assert res["checked"] is True
    assert res["deleted"] == 19
    assert res["remaining_excess"] == 1
    assert res["skipped_cooldown"] is False
    assert sorted(p.name for p in log_dir.iterdir()) == sorted([blocked_name, *created[20:]])

    # (b) Cooldown-Stamp wurde auch im Teilfehlerfall gesetzt und verhindert sofortigen Re-Scan.
    stamp = pdir / la.AGY_PROFILE_LOG_PRUNE_STAMP_NAME
    assert stamp.is_file()
    res_cooldown = la.prune_profile_logs(pdir, max_files=100, batch_limit=1000, cooldown_s=600.0, now=now + 60.0)
    assert res_cooldown["skipped_cooldown"] is True

    # (c) Fehleranzahl und Fehlermeldung sind im Ergebnis dokumentiert.
    assert res["unlink_errors"] == 1
    assert res["error"] is not None
    assert "Permission denied" in res["error"]


def test_profile_log_max_age_removes_stale_files_except_active_cli_log_target(tmp_path):
    pdir = tmp_path / "profile"
    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = 1_800_000_000.0
    old_names = _make_cli_logs(log_dir, 5, base_epoch=now - 5 * la.DAY_S, step_s=10.0)
    fresh_names = _make_cli_logs(log_dir, 3, base_epoch=now - 3600, step_s=10.0)

    # Aktiver cli.log-Symlink zeigt auf eine ältere Datei -> diese bleibt geschützt.
    os.symlink(f"log/{old_names[0]}", log_dir.parent / "cli.log")

    res = la.prune_profile_logs(pdir, max_files=100, max_age_s=3 * la.DAY_S, now=now)

    assert res["checked"] is True
    assert res["deleted"] == 4
    assert sorted(p.name for p in log_dir.iterdir()) == sorted([old_names[0], *fresh_names])


def test_profile_log_readonly_paths_are_strictly_fail_open(tmp_path):
    if os.geteuid() == 0:
        pytest.skip("Root ignoriert POSIX-Read-Only-Bits")

    pdir = tmp_path / "profile"
    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = 1_800_000_000.0
    _make_cli_logs(log_dir, 120, base_epoch=now - 1200, step_s=10.0)

    try:
        os.chmod(log_dir, 0o555)
        os.chmod(pdir, 0o555)
        res = la.prune_profile_logs(pdir, max_files=100, now=now)
        assert res["deleted"] == 0
        assert res["unlink_errors"] == 20
        assert res["error"] is not None
        assert len(list(log_dir.iterdir())) == 120
    finally:
        os.chmod(pdir, 0o700)
        os.chmod(log_dir, 0o700)


def test_events_jsonl_compacts_atomically_to_last_n_lines_when_over_line_cap(tmp_path):
    events_path = tmp_path / "events.jsonl"
    records = [{"seq": i, "event": "switch"} for i in range(120)]
    events_path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")

    # Unterhalb des Deckels: keine Änderung
    assert la.compact_events_jsonl(events_path, max_bytes=10_000_000, max_lines=120, keep_lines=50) is False
    assert len(events_path.read_text(encoding="utf-8").splitlines()) == 120

    # Oberhalb des Deckels: atomare Kürzung auf genau die letzten 50 Zeilen
    assert la.compact_events_jsonl(events_path, max_bytes=10_000_000, max_lines=100, keep_lines=50) is True
    kept = [json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines()]
    assert len(kept) == 50
    assert [r["seq"] for r in kept] == list(range(70, 120))
    assert stat.S_IMODE(events_path.stat().st_mode) == 0o600
    # Keine liegengebliebenen Temp-Dateien
    leftovers = [p.name for p in tmp_path.iterdir() if p.name.startswith(".events.jsonl.")]
    assert leftovers == []


def test_events_jsonl_compacts_when_over_byte_cap(tmp_path):
    events_path = tmp_path / "events.jsonl"
    payload = "x" * 180
    lines = [json.dumps({"seq": i, "pad": payload}) + "\n" for i in range(30)]
    events_path.write_text("".join(lines), encoding="utf-8")
    assert events_path.stat().st_size > 2000

    assert la.compact_events_jsonl(events_path, max_bytes=2000, max_lines=5000, keep_lines=2500) is True
    assert events_path.stat().st_size <= 2000
    kept = [json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines()]
    assert kept[-1]["seq"] == 29
    assert [r["seq"] for r in kept] == list(range(30 - len(kept), 30))


def test_events_jsonl_symlink_and_readonly_are_fail_open(tmp_path):
    if os.geteuid() == 0:
        pytest.skip("Root ignoriert POSIX-Read-Only-Bits")

    real_file = tmp_path / "real.jsonl"
    real_file.write_text("".join(f'{{"seq":{i}}}\n' for i in range(30)), encoding="utf-8")
    link = tmp_path / "events.jsonl"
    os.symlink(str(real_file), link)
    assert la.compact_events_jsonl(link, max_bytes=10, max_lines=5, keep_lines=2) is False
    assert len(real_file.read_text(encoding="utf-8").splitlines()) == 30

    # events.lock vorab anlegen, damit _events_lock das Read-only-Verzeichnis übersteht
    # und erst atomic_write (mkstemp) wirklich am Read-only-Verzeichnis scheitert.
    ro_dir = tmp_path / "readonly"
    ro_dir.mkdir()
    ro_events = ro_dir / "events.jsonl"
    ro_events.write_text("".join(f'{{"seq":{i}}}\n' for i in range(30)), encoding="utf-8")
    ro_lock = ro_dir / la.EVENTS_LOCK_FILE
    ro_lock.write_bytes(b"")
    os.chmod(ro_lock, 0o600)
    try:
        os.chmod(ro_dir, 0o555)
        assert la.compact_events_jsonl(ro_events, max_bytes=10, max_lines=5, keep_lines=2) is False
        assert len(ro_events.read_text(encoding="utf-8").splitlines()) == 30
    finally:
        os.chmod(ro_dir, 0o700)


def test_events_jsonl_atomic_write_failure_is_fail_open_for_compact_and_log_event(env_fixture, monkeypatch):
    monkeypatch.setattr(la, "EVENTS_MAX_LINES", 5)
    monkeypatch.setattr(la, "EVENTS_KEEP_LINES", 2)

    env_fixture.state.mkdir(parents=True, exist_ok=True)
    events_path = env_fixture.state / la.EVENTS_FILE
    events_path.write_text("".join(json.dumps({"seq": i, "event": "seed"}) + "\n" for i in range(6)), encoding="utf-8")

    calls = {"n": 0}

    def failing_atomic_write(path, data, **kwargs):
        calls["n"] += 1
        raise OSError(errno.ENOSPC, "No space left on device")

    monkeypatch.setattr(la, "atomic_write", failing_atomic_write)

    # Direkte Kompaktierung erreicht atomic_write, schlägt fail-open fehl und verliert keine Daten.
    assert la.compact_events_jsonl(events_path, max_bytes=10_000_000, max_lines=5, keep_lines=2) is False
    assert calls["n"] == 1
    assert len(events_path.read_text(encoding="utf-8").splitlines()) == 6

    # log_event schreibt die neue Zeile, Kompaktierung scheitert in atomic_write -> fail-open ohne Datenverlust/Duplikat.
    la.log_event("after_fail", seq=99)
    assert calls["n"] == 2
    rows = [json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines()]
    assert len(rows) == 7
    assert rows[-1]["event"] == "after_fail"
    assert rows[-1]["seq"] == 99


def test_events_and_stamp_o_nofollow_enforced_directly_in_reader_and_writer(env_fixture, tmp_path):
    # 1) Leser (_compact_events_jsonl_locked) umgeht target.is_symlink() und prüft direkt O_NOFOLLOW.
    real_events = tmp_path / "outside-events.jsonl"
    real_events.write_text("".join(f'{{"seq":{i}}}\n' for i in range(25)), encoding="utf-8")
    link_events = tmp_path / "events-link.jsonl"
    os.symlink(str(real_events), link_events)
    assert la._compact_events_jsonl_locked(link_events, max_bytes=10, max_lines=5, keep_lines=2) is False
    assert len(real_events.read_text(encoding="utf-8").splitlines()) == 25

    # 2) Lock (_events_lock) weist Symlink direkt über O_NOFOLLOW ab (ELOOP).
    real_lock = tmp_path / "outside.lock"
    real_lock.write_text("keep", encoding="utf-8")
    link_lock = tmp_path / "events.lock"
    os.symlink(str(real_lock), link_lock)
    with pytest.raises(OSError) as lock_exc:
        with la._events_lock(link_lock):
            pass
    assert lock_exc.value.errno == errno.ELOOP
    assert real_lock.read_text(encoding="utf-8") == "keep"

    # 3) Schreiber (log_event) weist Symlink an events.jsonl direkt über O_NOFOLLOW ab (ELOOP), Ziel bleibt unangetastet.
    env_fixture.state.mkdir(parents=True, exist_ok=True)
    outside_target = tmp_path / "outside-target.jsonl"
    outside_target.write_text("untouched\n", encoding="utf-8")
    state_events = env_fixture.state / la.EVENTS_FILE
    if state_events.exists() or state_events.is_symlink():
        state_events.unlink()
    os.symlink(str(outside_target), state_events)
    with pytest.raises(OSError) as write_exc:
        la.log_event("symlink_probe")
    assert write_exc.value.errno == errno.ELOOP
    assert outside_target.read_text(encoding="utf-8") == "untouched\n"

    # 4) Stamp-Schreiber (_touch_prune_stamp_safe) weist Symlink über O_NOFOLLOW ab.
    outside_stamp = tmp_path / "outside-stamp.txt"
    outside_stamp.write_text("preserve-me", encoding="utf-8")
    link_stamp = tmp_path / ".log-prune.stamp"
    os.symlink(str(outside_stamp), link_stamp)
    la._touch_prune_stamp_safe(link_stamp, 1_800_000_000.0)
    assert outside_stamp.read_text(encoding="utf-8") == "preserve-me"


def test_touch_prune_stamp_safe_does_not_block_on_fifo(tmp_path):
    fifo = tmp_path / ".log-prune.stamp"
    os.mkfifo(fifo)
    t0 = time.monotonic()
    la._touch_prune_stamp_safe(fifo, 1_800_000_000.0)
    elapsed = time.monotonic() - t0
    assert elapsed < 0.5
    assert stat.S_ISFIFO(os.lstat(fifo).st_mode)


def test_events_lock_times_out_nonblocking_and_falls_back_fail_open(env_fixture, monkeypatch):
    monkeypatch.setattr(la, "EVENTS_LOCK_TIMEOUT_S", 0.08)
    monkeypatch.setattr(la, "EVENTS_LOCK_POLL_S", 0.01)

    env_fixture.state.mkdir(parents=True, exist_ok=True)
    events_path = env_fixture.state / la.EVENTS_FILE
    events_path.write_text("".join(json.dumps({"seq": i}) + "\n" for i in range(10)), encoding="utf-8")
    lock_path = env_fixture.state / la.EVENTS_LOCK_FILE
    holder_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(holder_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)

        # Direkter Lock-Versuch bricht nach kurzer Retry-Frist mit BlockingIOError ab.
        t0 = time.monotonic()
        with pytest.raises(BlockingIOError):
            with la._events_lock(lock_path, timeout_s=0.06):
                pass
        assert time.monotonic() - t0 < 0.5

        # compact_events_jsonl bleibt fail-open und blockiert nicht.
        t1 = time.monotonic()
        assert la.compact_events_jsonl(events_path, max_bytes=10, max_lines=5, keep_lines=2) is False
        assert time.monotonic() - t1 < 0.5

        # log_event fällt nach Lock-Timeout auf den ungesperrten Append-Fallback zurück statt zu blockieren.
        t2 = time.monotonic()
        la.log_event("during_busy_lock", seq=42)
        assert time.monotonic() - t2 < 0.5
        rows = [json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines()]
        assert rows[-1]["event"] == "during_busy_lock"
        assert rows[-1]["seq"] == 42
    finally:
        try:
            fcntl.flock(holder_fd, fcntl.LOCK_UN)
        finally:
            os.close(holder_fd)


def test_log_event_triggers_compaction_automatically(env_fixture, monkeypatch):
    monkeypatch.setattr(la, "EVENTS_MAX_LINES", 20)
    monkeypatch.setattr(la, "EVENTS_KEEP_LINES", 10)

    for i in range(21):
        la.log_event("tick", seq=i)

    lines = [json.loads(line) for line in (env_fixture.state / "events.jsonl").read_text(encoding="utf-8").splitlines()]
    assert len(lines) == 10
    assert [r["seq"] for r in lines] == list(range(11, 21))


def test_autoswitch_non_dry_run_triggers_profile_log_retention_while_dry_run_skips(env_fixture):
    """Belegt den echten Non-Dry-Run-Prune in cmd_auto_switch (auch auf Cache-Fast-Path ohne probe_slots)."""
    write_account(env_fixture.live, "a@example.test")
    assert la.main(["save", "agy", "acc1", "--label", "personal"]) == 0
    env_fixture.usage.write_text(json.dumps({"a@example.test": tsv(90, 90, 90, 90)}), encoding="utf-8")

    # Frischen quotas.json-Cache anlegen, damit cmd_auto_switch den Cache-Fast-Path (probe_used=False) nimmt
    # und das Prunen ausschließlich über cmd_auto_switch -> _prune_provider_logs_safe -> prune_slot_logs erfolgt.
    la.fetch_provider_status(env_fixture.adapter, write_quotas=True)
    pdir = la.profile_root(env_fixture.adapter, "acc1")
    stamp = pdir / la.AGY_PROFILE_LOG_PRUNE_STAMP_NAME
    if stamp.exists():
        stamp.unlink()

    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = time.time()
    created = _make_cli_logs(log_dir, 130, base_epoch=now - 1300, step_s=1.0)

    # 1) dry_run=True: 0 Dateien gelöscht, kein Stamp geschrieben.
    out_dry = json.loads(la.cmd_auto_switch(env_fixture.adapter, as_json=True, dry_run=True, no_bounce=True))
    assert out_dry["decision"] == "stay_healthy"
    assert out_dry["dry_run"] is True
    assert out_dry["probe_used"] is False
    assert len(list(log_dir.iterdir())) == 130
    assert not stamp.exists()

    # 2) dry_run=False: cmd_auto_switch löst prune_slot_logs aus, baut die 30 ältesten Logs ab und schreibt Stamp.
    out_live = json.loads(la.cmd_auto_switch(env_fixture.adapter, as_json=True, dry_run=False, no_bounce=True))
    assert out_live["decision"] == "stay_healthy"
    assert out_live["dry_run"] is False
    assert out_live["probe_used"] is False
    assert sorted(p.name for p in log_dir.iterdir()) == sorted(created[-100:])
    assert stamp.is_file()
    assert (pdir / la.AgyAdapter.token_rel).is_file()
    assert (env_fixture.state / "accounts.json").is_file()


def test_heal_and_probe_trigger_profile_log_retention(env_fixture):
    write_account(env_fixture.live, "a@example.test")
    assert la.main(["save", "agy", "acc1", "--label", "personal"]) == 0
    env_fixture.usage.write_text(json.dumps({"a@example.test": tsv(90, 90, 90, 90)}), encoding="utf-8")

    pdir = la.profile_root(env_fixture.adapter, "acc1")
    log_dir = pdir / la.AgyAdapter.log_dir_rel
    now = time.time()
    created = _make_cli_logs(log_dir, 130, base_epoch=now - 1300, step_s=1.0)

    # cmd_heal räumt die 30 ältesten Dateien ab; Token und accounts.json bleiben heil.
    assert la.cmd_heal(env_fixture.adapter) in {"current", "synced"}
    assert sorted(p.name for p in log_dir.iterdir()) == sorted(created[-100:])
    assert (pdir / la.AgyAdapter.token_rel).is_file()
    assert (env_fixture.state / "accounts.json").is_file()

    # Stamp entfernen, weitere Logs anlegen -> probe_slot räumt nach Probe ebenfalls auf.
    (pdir / la.AGY_PROFILE_LOG_PRUNE_STAMP_NAME).unlink()
    more = _make_cli_logs(log_dir, 120, base_epoch=now - 600, step_s=1.0)
    probe = la.probe_slot(env_fixture.adapter, "acc1")
    assert probe["auth_state"] == la.AUTH_OK
    assert len(list(log_dir.iterdir())) == 100
    assert more[-1] in {p.name for p in log_dir.iterdir()}


def test_heal_clobber_recovery_compacts_events_without_deadlock(env_fixture, monkeypatch):
    """log_event wird aus heal_locked unter switch_lock aufgerufen und darf nicht mit events.lock verklemmen."""
    write_account(env_fixture.live, "a@example.test")
    assert la.main(["save", "agy", "acc1", "--label", "personal"]) == 0
    write_account(env_fixture.live, "b@example.test")
    assert la.main(["save", "agy", "acc2", "--label", "work"]) == 0

    monkeypatch.setattr(la, "EVENTS_MAX_LINES", 10)
    monkeypatch.setattr(la, "EVENTS_KEEP_LINES", 5)

    events_path = env_fixture.state / "events.jsonl"
    events_path.write_text("".join(json.dumps({"seq": i, "event": "old"}) + "\n" for i in range(10)), encoding="utf-8")

    # Clobber provozieren: Live-Token gehört zu acc1, aktiv ist acc2.
    write_account(env_fixture.live, "a@example.test", marker="-CLOBBER")
    assert la.cmd_heal(env_fixture.adapter) == "clobber_recovered"

    kept = [json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines()]
    assert len(kept) == 5
    assert kept[-1]["event"] == "clobber_recovered"

