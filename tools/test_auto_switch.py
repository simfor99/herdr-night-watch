"""Tests für `llm-account auto-switch`, `install-autoswitch-timer` und `--reason quota_reached`.

Vollständig hermetisch: Fake-agy, Fake-Herdr, Fake-/proc, temporäres State-/Home-Verzeichnis.
"""

# ruff: noqa: F811  (pytest-Fixtures werden aus test_bounce_stale/test_llm_account importiert)

from __future__ import annotations

import fcntl
import json
import os
import sys
import time
from pathlib import Path

import pytest

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import llm_account as la  # noqa: E402
from test_bounce_stale import (  # noqa: E402,F401  (Fixtures und Helfer)
    BTIME,
    CONV_OLD,
    CONV_OTHER,
    EMPTY_SCREEN,
    MODEL,
    SWITCH_EPOCH,
    TICKS,
    FakeHerdr,
    add_agy,
    fake_chat,
    pane,
    world,
)
from test_llm_account import env, set_usage, tsv, write_account  # noqa: E402,F401


RESET_EARLIER = "2026-11-28T08:00:00Z"
RESET_LATER = "2026-11-30T08:00:00Z"
RESET_5H = "2026-11-22T18:00:00Z"


def _setup_two_slots(env, *, active="acc2", switch_epoch=1000.0):
    """Registriert acc1 (späterer Reset) und acc2 (früherer Reset), mit aktivem Live-Home."""
    write_account(env.live, "a@example.test")
    assert la.main(["save", "agy", "acc1", "--label", "Acc1"]) == 0
    write_account(env.live, "b@example.test")
    assert la.main(["save", "agy", "acc2", "--label", "Acc2"]) == 0
    if active != "acc2":
        assert la.main(["use", "agy", active]) == 0
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = switch_epoch
    reg_path.write_text(json.dumps(reg, indent=2) + "\n", encoding="utf-8")
    return reg["providers"]["agy"]


def _acc_entry(
    slot_id: str,
    *,
    weekly_pct: int | None,
    five_hour_pct: int | None,
    weekly_reset: str | None,
    auth_state: str = la.AUTH_OK,
    is_active: bool = False,
) -> dict:
    return {
        "id": slot_id,
        "label": slot_id,
        "email": f"{slot_id}@example.test",
        "state": la.ACTIVE,
        "is_active": is_active,
        "auth_state": auth_state,
        "lanes": {
            "gemini": {
                "five_hour_pct": five_hour_pct,
                "five_hour_reset": RESET_5H,
                "weekly_pct": weekly_pct,
                "weekly_reset": weekly_reset,
            },
            "claude_gpt": {
                "five_hour_pct": five_hour_pct,
                "five_hour_reset": RESET_5H,
                "weekly_pct": weekly_pct,
                "weekly_reset": weekly_reset,
            },
        },
    }


def _write_quotas(env, *, active: str, switch_epoch: float, updated_epoch: float, accounts: list[dict]) -> Path:
    snap = {
        "version": 1,
        "updated_epoch": updated_epoch,
        "heal": "current",
        "providers": {
            "agy": {
                "active_account": active,
                "switch_epoch": switch_epoch,
                "primary_lane": "gemini",
                "accounts": accounts,
                "effective": {"lanes": {}},
            }
        },
    }
    env.state.mkdir(parents=True, exist_ok=True)
    path = env.state / "quotas.json"
    path.write_text(json.dumps(snap, indent=2) + "\n", encoding="utf-8")
    os.chmod(path, 0o600)
    return path


def _read_registry_block(env) -> dict:
    return json.loads((env.state / "accounts.json").read_text(encoding="utf-8"))["providers"]["agy"]


# ---------------------------------------------------------------------------
# 1. DRAIN-TO-ZERO
# ---------------------------------------------------------------------------


def test_drain_to_zero_stays_on_active_at_14_pct_weekly_and_50_pct_5h(env, monkeypatch):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 14, 50, 14, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "stay_healthy"
    assert out["active_account"] == "acc2"
    assert out["target_slot"] is None
    assert _read_registry_block(env)["active_account"] == "acc2"


@pytest.mark.parametrize("g5,gw,expected_reason", [
    (50, 2, "weekly_exhausted"),
    (50, 1, "weekly_exhausted"),
    (50, 0, "weekly_exhausted"),
    (2, 60, "five_hour_exhausted"),
    (1, 60, "five_hour_exhausted"),
    (0, 60, "five_hour_exhausted"),
])
def test_drain_to_zero_switches_at_or_below_2_pct(env, g5, gw, expected_reason):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(80, 90, 80, 90, resetw=RESET_LATER),
        "b@example.test": tsv(g5, gw, g5, gw, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "switched"
    assert out["previous_account"] == "acc2"
    assert out["target_slot"] == "acc1"
    assert out["active_account"] == "acc1"
    assert out["trigger_reason"] == expected_reason
    assert _read_registry_block(env)["active_account"] == "acc1"


@pytest.mark.parametrize("g5,gw", [
    (50, 3),
    (3, 60),
])
def test_drain_to_zero_stays_healthy_at_3_pct_weekly_or_5h(env, g5, gw):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(g5, gw, g5, gw, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "stay_healthy"
    assert out["active_account"] == "acc2"
    assert out["target_slot"] is None
    assert _read_registry_block(env)["active_account"] == "acc2"


def test_drain_to_zero_switches_at_2_pct_weekly_or_2_pct_5h(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 2, 50, 2, resetw=RESET_EARLIER),
    })

    out_weekly = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert out_weekly["decision"] == "switched"
    assert out_weekly["trigger_reason"] == "weekly_exhausted"
    assert out_weekly["active_account"] == "acc1"

    assert la.main(["use", "agy", "acc2"]) == 0
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = time.time() - 600
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(2, 60, 2, 60, resetw=RESET_EARLIER),
    })

    out_5h = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert out_5h["decision"] == "switched"
    assert out_5h["trigger_reason"] == "five_hour_exhausted"
    assert out_5h["active_account"] == "acc1"


def test_drain_to_zero_switches_on_auth_error_or_reactive_exhausted(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    # b@example.test fehlt in usage.json -> token expired -> AUTH_RELOGIN
    set_usage(env, {
        "a@example.test": tsv(80, 90, 80, 90, resetw=RESET_LATER),
    })
    out_auth = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert out_auth["decision"] == "switched"
    assert out_auth["trigger_reason"] == "auth_relogin_required"
    assert out_auth["target_slot"] == "acc1"

    # Zurück auf acc2 mit genügend Restquote, aber --reactive-exhausted gesetzt
    assert la.main(["use", "agy", "acc2"]) == 0
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = time.time() - 600
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    set_usage(env, {
        "a@example.test": tsv(80, 90, 80, 90, resetw=RESET_LATER),
        "b@example.test": tsv(10, 40, 10, 40, resetw=RESET_EARLIER),
    })
    out_reactive = json.loads(
        la.cmd_auto_switch(env.adapter, as_json=True, reactive_exhausted=True, no_bounce=True)
    )
    assert out_reactive["decision"] == "switched"
    assert out_reactive["trigger_reason"] == "reactive_quota_reached"
    assert out_reactive["target_slot"] == "acc1"


# ---------------------------------------------------------------------------
# 2. FEFO-Rückwechsel
# ---------------------------------------------------------------------------


def test_fefo_return_switch_flow_and_guards(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 1000)

    # Schritt 1: acc2 5h erschöpft (weekly 68% > 2%) -> Wechsel acc2 -> acc1
    set_usage(env, {
        "a@example.test": tsv(95, 90, 95, 90, resetw=RESET_LATER),
        "b@example.test": tsv(1, 68, 1, 68, resetw=RESET_EARLIER),
    })
    step1 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert step1["decision"] == "switched"
    assert step1["active_account"] == "acc1"

    # Cooldown für Folgeschritte zurücksetzen
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = time.time() - 600
    reg_path.write_text(json.dumps(reg), encoding="utf-8")

    # Schritt 2: acc2 hat 5h=19% (< 20%) -> bleibt auf acc1
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(19, 68, 19, 68, resetw=RESET_EARLIER),
    })
    step2 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step2["decision"] == "stay_healthy"
    assert step2["active_account"] == "acc1"

    # Schritt 3: acc2 hat 5h=20% (>= 20%), weekly=68% (> 2%) und früheres weekly_reset -> Rückwechsel acc1 -> acc2
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(20, 68, 20, 68, resetw=RESET_EARLIER),
    })
    step3 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step3["decision"] == "switched"
    assert step3["previous_account"] == "acc1"
    assert step3["active_account"] == "acc2"
    assert step3["trigger_reason"] == "fefo_earlier_reset_ready"


@pytest.mark.parametrize("earlier_weekly", [0, 1, 2])
def test_fefo_does_not_return_when_earlier_slot_weekly_is_at_or_below_2_pct(env, earlier_weekly):
    _setup_two_slots(env, active="acc1", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(100, earlier_weekly, 100, earlier_weekly, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "stay_healthy"
    assert out["active_account"] == "acc1"


def test_fefo_anti_ping_pong_ignores_2_pct_and_returns_only_above_2_pct_and_20_pct_5h(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 1000)

    # Schritt 1: acc2 bei weekly=2% (5h=80%) -> Drain-Switch auf acc1
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(80, 2, 80, 2, resetw=RESET_EARLIER),
    })
    step1 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert step1["decision"] == "switched"
    assert step1["active_account"] == "acc1"
    assert step1["trigger_reason"] == "weekly_exhausted"

    # Cooldown abgelaufen, acc2 immer noch bei weekly=2% -> KEIN Rückwechsel (Anti-Ping-Pong)
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = time.time() - 600
    reg_path.write_text(json.dumps(reg), encoding="utf-8")

    step2 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step2["decision"] == "stay_healthy"
    assert step2["active_account"] == "acc1"

    # acc2 hat weekly=3% (> 2%), aber 5h=19% (< 20%) -> bleibt auf acc1
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(19, 3, 19, 3, resetw=RESET_EARLIER),
    })
    step3 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step3["decision"] == "stay_healthy"
    assert step3["active_account"] == "acc1"

    # acc2 hat weekly=3% (> 2%) und 5h=20% (>= 20%) -> FEFO-Rückwechsel acc1 -> acc2
    set_usage(env, {
        "a@example.test": tsv(80, 85, 80, 85, resetw=RESET_LATER),
        "b@example.test": tsv(20, 3, 20, 3, resetw=RESET_EARLIER),
    })
    step4 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step4["decision"] == "switched"
    assert step4["active_account"] == "acc2"
    assert step4["trigger_reason"] == "fefo_earlier_reset_ready"

    # Nach Rückwechsel bleibt acc2 bei weekly=3%, 5h=20% stabil aktiv (kein Ping-Pong zurück auf acc1)
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = time.time() - 600
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    step5 = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, force_probe=True, no_bounce=True))
    assert step5["decision"] == "stay_healthy"
    assert step5["active_account"] == "acc2"


# ---------------------------------------------------------------------------
# 3. Zero Ping-Pong: Cooldown, Sub-Floor-Hysterese, All-Exhausted, Dry-Run, CAS
# ---------------------------------------------------------------------------


def test_cooldown_prevents_ping_pong_switch(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 30)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, cooldown=300, no_bounce=True))

    assert out["decision"] == "cooldown_active"
    assert 200 <= out["cooldown_remaining_s"] <= 300
    assert out["target_slot"] == "acc1"
    assert _read_registry_block(env)["active_account"] == "acc2"


def test_sub_floor_accounts_do_not_ping_pong(env):
    # Beide unter Floor (15/10), aber > 2%: stay_healthy, kein Hin-und-Her-Wechseln
    accounts = [
        _acc_entry("acc1", weekly_pct=12, five_hour_pct=9, weekly_reset=RESET_LATER, is_active=False),
        _acc_entry("acc2", weekly_pct=3, five_hour_pct=3, weekly_reset=RESET_EARLIER, is_active=True),
    ]
    res = la._evaluate_auto_switch_decision(accounts, "acc2", "gemini")
    assert res["action"] == "stay_healthy"

    # Bei vollständiger Erschöpfung aller Slots (<= 2%) -> all_slots_exhausted
    for pct in (0, 1, 2):
        exhausted = [
            _acc_entry("acc1", weekly_pct=pct, five_hour_pct=50, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=pct, weekly_reset=RESET_EARLIER, is_active=True),
        ]
        res_ex = la._evaluate_auto_switch_decision(exhausted, "acc2", "gemini")
        assert res_ex["action"] == "all_slots_exhausted"


def test_all_slots_exhausted_cli_returns_exit_0_without_switching(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(1, 50, 1, 50, resetw=RESET_LATER),
        "b@example.test": tsv(50, 1, 50, 1, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert out["decision"] == "all_slots_exhausted"
    assert _read_registry_block(env)["active_account"] == "acc2"


def test_dry_run_writes_nothing_and_reports_candidate(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })
    before_reg = (env.state / "accounts.json").read_text(encoding="utf-8")
    before_events = (env.state / "events.jsonl").read_text(encoding="utf-8") if (env.state / "events.jsonl").exists() else ""

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, dry_run=True, no_bounce=True))

    assert out["decision"] == "dry_run_would_switch"
    assert out["target_slot"] == "acc1"
    assert not (env.state / "quotas.json").exists()
    assert not (env.state / "auto-switch-bounce.lock").exists()
    assert (env.state / "accounts.json").read_text(encoding="utf-8") == before_reg
    after_events = (env.state / "events.jsonl").read_text(encoding="utf-8") if (env.state / "events.jsonl").exists() else ""
    assert after_events == before_events


def test_registry_changed_during_probe_skips_switch(env, monkeypatch):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)

    def racing_probes(adapter, slots):
        la.cmd_retire(adapter, "acc1", force=False)
        return {
            "acc1": {"auth_state": la.AUTH_OK, "lanes": la.parse_usage_tsv("\n".join(tsv(90, 90, 90, 90)))},
            "acc2": {"auth_state": la.AUTH_OK, "lanes": la.parse_usage_tsv("\n".join(tsv(0, 60, 0, 60)))},
        }

    monkeypatch.setattr(la, "probe_slots", racing_probes)
    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert out["decision"] == "skipped_registry_changed"


def test_dry_run_without_cache_heals_neither_live_token_nor_profiles(env, monkeypatch):
    """CodeRabbit MAJOR #1: Dry-Run darf trotz fehlendem Cache und vertauschtem Live-Token nie healen."""
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    # Vertauschtes Live-Token: Registry führt acc2, das Live-Home trägt den Token von acc1.
    write_account(env.live, "a@example.test")
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })
    assert not (env.state / "quotas.json").exists()  # Cache fehlt -> Live-Probe-Pfad
    before_reg = (env.state / "accounts.json").read_text(encoding="utf-8")
    before_live = (env.live / env.adapter.token_rel).read_bytes()
    before_profiles = {
        slot: (la.profile_root(env.adapter, slot) / env.adapter.token_rel).read_bytes()
        for slot in ("acc1", "acc2")
    }
    heal_calls = []
    monkeypatch.setattr(la, "heal_locked", lambda adapter: heal_calls.append(adapter) or "current")

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, dry_run=True, no_bounce=True))

    assert out["decision"] == "dry_run_would_switch"
    assert out["probe_used"] is True
    assert heal_calls == []  # Phase 1, Phase 2 und fetch_provider_status bleiben rein lesend
    assert (env.live / env.adapter.token_rel).read_bytes() == before_live
    assert (env.state / "accounts.json").read_text(encoding="utf-8") == before_reg
    assert not (env.state / "quotas.json").exists()
    for slot, raw in before_profiles.items():
        assert (la.profile_root(env.adapter, slot) / env.adapter.token_rel).read_bytes() == raw


def test_committed_switch_survives_quota_sync_error_and_skips_phase3(env, monkeypatch):
    """Ein Quota-Sync-Fehler nach verbindlichem Wechsel wird gemeldet, statt den Switch zu verwerfen."""
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })

    def failing_sync(*args, **kwargs):
        raise OSError("quotas.json nicht schreibbar")

    monkeypatch.setattr(la, "_sync_quotas_after_switch", failing_sync)
    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=False))

    assert out["decision"] == "switched"
    assert out["target_slot"] == "acc1"
    assert out["active_account"] == "acc1"
    assert "quotas.json nicht schreibbar" in out["quota_sync_error"]
    assert out["bounce_summary"]["skipped_reason"] == "quota_sync_failed"
    assert out["bounces"] == []
    assert _read_registry_block(env)["active_account"] == "acc1"


def test_auto_switch_logs_no_secrets_tokens_or_emails(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })

    la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True)
    events_raw = (env.state / "events.jsonl").read_text(encoding="utf-8")
    assert "SECRET-" not in events_raw
    assert "a@example.test" not in events_raw
    assert "b@example.test" not in events_raw
    assert '"event": "auto_switch"' in events_raw


@pytest.mark.parametrize("argv", [
    ["auto-switch", "agy", "--max-quota-age", "nan"],
    ["auto-switch", "agy", "--max-quota-age", "9"],
    ["auto-switch", "agy", "--max-quota-age", "3601"],
    ["auto-switch", "agy", "--cooldown", "inf"],
    ["auto-switch", "agy", "--cooldown", "-1"],
    ["auto-switch", "agy", "--max-bounces", "11"],
    ["auto-switch", "agy", "--quiet-recheck", "nan"],
    ["auto-switch", "agy", "--quiet-recheck", "301"],
])
def test_auto_switch_rejects_invalid_args_with_exit_2(env, argv):
    assert la.main(argv) == 2


# ---------------------------------------------------------------------------
# 4. S1-1: Cache-Fast-Path & Live-Verify vor echtem Switch
# ---------------------------------------------------------------------------


def test_cache_aged_while_waiting_for_switch_lock_reprobes_once(env, monkeypatch):
    """Cache-Recheck unter switch_lock: Ist der Snapshot beim Lock-Erwerb zu alt, folgt genau ein Live-Probe."""
    block = _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 50, 50, 50, resetw=RESET_EARLIER),
    })
    _write_quotas(
        env,
        active="acc2",
        switch_epoch=block["switch_epoch"],
        updated_epoch=time.time(),
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    calls = {"n": 0}
    real_check = la._cache_snapshot_matches_block

    def aging_cache(snap, adapter, block, now, max_quota_age):
        calls["n"] += 1
        ok, age = real_check(snap, adapter, block, now, max_quota_age)
        if calls["n"] == 2:  # Recheck unter switch_lock: Snapshot ist inzwischen zu alt
            return False, age
        return ok, age

    monkeypatch.setattr(la, "_cache_snapshot_matches_block", aging_cache)
    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert calls["n"] == 2  # Phase 1 ok, Phase 2 zu alt -> genau einmal Live-Probe, kein dritter Check
    assert out["probe_used"] is True
    assert out["decision"] == "stay_healthy"


def test_cache_fast_path_stay_healthy_never_calls_probe_slot(env, monkeypatch):
    block = _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    _write_quotas(
        env,
        active="acc2",
        switch_epoch=block["switch_epoch"],
        updated_epoch=time.time() - 20,
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=40, five_hour_pct=40, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    probe_calls = []
    monkeypatch.setattr(la, "probe_slot", lambda *a, **k: probe_calls.append(a))

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "stay_healthy"
    assert out["probe_used"] is False
    assert probe_calls == []


def test_cache_switch_trigger_forces_live_probe_and_aborts_if_slot_reset(env, monkeypatch):
    block = _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    # Veralteter/vorläufiger Cache behauptet acc2 5h=1% -> Wechselkandidat!
    _write_quotas(
        env,
        active="acc2",
        switch_epoch=block["switch_epoch"],
        updated_epoch=time.time() - 10,
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=60, five_hour_pct=1, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    # Live-Probe zeigt jedoch: acc2 wurde gerade auf 100% 5h resettet!
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(100, 60, 100, 60, resetw=RESET_EARLIER),
    })
    orig_probe = la.probe_slot
    probe_calls = []

    def spy_probe(adapter, slot_id):
        probe_calls.append(slot_id)
        return orig_probe(adapter, slot_id)

    monkeypatch.setattr(la, "probe_slot", spy_probe)

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["probe_used"] is True
    assert set(probe_calls) == {"acc1", "acc2"}
    assert out["decision"] == "stay_healthy"
    assert _read_registry_block(env)["active_account"] == "acc2"


def test_stale_panes_with_cache_age_over_120s_triggers_fresh_probe(world, monkeypatch):
    block = _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    add_agy(world.proc, 4242, "w1:p1", start_epoch=SWITCH_EPOCH - 50, cwd=world.cwd)
    _write_quotas(
        world.env,
        active="acc2",
        switch_epoch=block["switch_epoch"],
        updated_epoch=time.time() - 135,  # > 120s, <= 180s
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    set_usage(world.env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 50, 50, 50, resetw=RESET_EARLIER),
    })
    herdr = FakeHerdr([pane(status="working")])  # blockt Bounce sauber, testet nur Probe-Trigger

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, herdr=herdr))

    assert out["probe_used"] is True
    assert out["decision"] == "stay_healthy"


# ---------------------------------------------------------------------------
# 5. Atomarer quotas.json-Sync nach Switch
# ---------------------------------------------------------------------------


def test_quotas_json_synced_atomically_so_disk_token_blockers_is_empty(env):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "switched"
    assert la.disk_token_blockers(env.adapter) == []
    snap = json.loads((env.state / "quotas.json").read_text(encoding="utf-8"))
    prov = snap["providers"]["agy"]
    assert prov["active_account"] == "acc1"
    assert prov["switch_epoch"] == out["switch_epoch"]
    flags = {a["id"]: a["is_active"] for a in prov["accounts"]}
    assert flags == {"acc1": True, "acc2": False}
    assert oct((env.state / "quotas.json").stat().st_mode & 0o777) == "0o600"


# ---------------------------------------------------------------------------
# 6. Phase 3: Opportunistischer Warm-Bounce
# ---------------------------------------------------------------------------


def _conv_uuid(idx: int) -> str:
    return f"11111111-2222-4333-8444-5555555555{idx:02d}"


def test_phase3_bounces_only_unfocused_idle_unprotected_panes_with_no_resume_prompt(world):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    set_usage(world.env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(0, 60, 0, 60, resetw=RESET_EARLIER),
    })
    # Fünf stale Panes mit jeweils eigener Konversation:
    # p1 (bauer, idle, unfocused) -> soll gebounct werden
    # p2 (focused=True) -> im 1. Tick wegen Focus-Settle geskippt
    # p3 (working) -> geskippt
    # p4 (architekt, idle, unfocused) -> soll beim Kontowechsel ebenfalls gebounct werden
    # p5 (vermarkter) -> geskippt
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), start_epoch=BTIME + 200, cwd=world.cwd)
    add_agy(world.proc, 4103, "w1:p3", conv=_conv_uuid(3), start_epoch=BTIME + 300, cwd=world.cwd)
    add_agy(world.proc, 4104, "w1:p4", conv=_conv_uuid(4), start_epoch=BTIME + 400, cwd=world.cwd)
    add_agy(world.proc, 4105, "w1:p5", conv=_conv_uuid(5), start_epoch=BTIME + 350, cwd=world.cwd)

    panes = [
        {**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t-bauer", "focused": False},
        {**pane(pane_id="w1:p2", conv=_conv_uuid(2), status="idle"), "tab_id": "t-bauer2", "focused": True},
        {**pane(pane_id="w1:p3", conv=_conv_uuid(3), status="working"), "tab_id": "t-bauer3", "focused": False},
        {**pane(pane_id="w1:p4", conv=_conv_uuid(4), status="idle"), "tab_id": "t-arch", "focused": False},
        {**pane(pane_id="w1:p5", conv=_conv_uuid(5), status="idle"), "tab_id": "t-human", "focused": False},
    ]
    labels = {
        "t-bauer": "bauer",
        "t-bauer2": "bauer",
        "t-bauer3": "bauer",
        "t-arch": "architekt",
        "t-human": "vermarkter",
    }

    class MultiHerdr(FakeHerdr):
        def tab_label(self, tab_id):
            return labels.get(tab_id)

        def run(self, pane_id, text):
            super().run(pane_id, text)
            pid_map = {"w1:p1": ("4101", 4999, _conv_uuid(1)), "w1:p4": ("4104", 4998, _conv_uuid(4))}
            old_pid, new_pid, conv = pid_map[pane_id]
            if text == "/exit":
                for sub in (world.proc / old_pid).iterdir():
                    sub.unlink()
                (world.proc / old_pid).rmdir()
            elif "agy --conversation" in text:
                add_agy(world.proc, new_pid, pane_id, conv=conv, start_epoch=int(time.time()), cwd=world.cwd)

    herdr = MultiHerdr(panes)
    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))

    assert out["decision"] == "switched"
    assert out["bounce_summary"]["executed"] == 2
    bounced = [b for b in out["bounces"] if b["status"] == "ok"]
    assert [b["pane_id"] for b in bounced] == ["w1:p1", "w1:p4"]
    assert all(b["wakeup_status"] == "disabled" for b in bounced)
    skipped = {b["pane_id"]: b for b in out["bounces"] if b["status"] == "skipped"}
    assert set(skipped) == {"w1:p2", "w1:p3", "w1:p5"}
    assert "pane_focused" in skipped["w1:p2"]["blockers"]
    assert skipped["w1:p3"]["reason"] == "live_blockers"
    assert "protected_human_tab" in skipped["w1:p5"]["blockers"]
    assert [c[0] for c in herdr.calls] == ["w1:p1", "w1:p1", "w1:p4", "w1:p4"]


def test_phase3_respects_max_bounces_and_start_epoch_order(world, monkeypatch):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    _write_quotas(
        world.env,
        active="acc2",
        switch_epoch=SWITCH_EPOCH,
        updated_epoch=time.time(),
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 300, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), start_epoch=BTIME + 100, cwd=world.cwd)  # älteste
    add_agy(world.proc, 4103, "w1:p3", conv=_conv_uuid(3), start_epoch=BTIME + 200, cwd=world.cwd)

    panes = [
        pane(pane_id="w1:p1", conv=_conv_uuid(1)),
        pane(pane_id="w1:p2", conv=_conv_uuid(2)),
        pane(pane_id="w1:p3", conv=_conv_uuid(3)),
    ]
    executed_panes = []
    monkeypatch.setattr(
        la,
        "execute_bounce",
        lambda adapter, plan, herdr, **kw: (
            executed_panes.append((plan["pane_id"], kw.get("resume_prompt")))
            or {**plan, "status": "ok", "new_pid": 9000, "stage": "start", "wakeup_status": "disabled"}
        ),
    )

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, max_bounces=2, herdr=FakeHerdr(panes)))

    assert executed_panes == [("w1:p2", False), ("w1:p3", False)]
    assert out["bounce_summary"]["executed"] == 2
    assert out["bounce_summary"]["stopped_reason"] == "max_bounces_reached"


def test_phase3_skips_when_bounce_lock_busy_and_isolates_wallclock_and_errors(world, monkeypatch):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    _write_quotas(
        world.env,
        active="acc2",
        switch_epoch=SWITCH_EPOCH,
        updated_epoch=time.time(),
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), start_epoch=BTIME + 200, cwd=world.cwd)

    # Test 1: bounce_lock busy
    lock_fd = os.open(world.env.state / "auto-switch-bounce.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        out_busy = json.loads(
            la.cmd_auto_switch(
                world.env.adapter,
                as_json=True,
                herdr=FakeHerdr([pane(pane_id="w1:p1", conv=_conv_uuid(1))]),
            )
        )
        assert out_busy["bounce_summary"]["skipped_reason"] == "bounce_lock_busy"
        assert out_busy["bounces"] == []
    finally:
        os.close(lock_fd)

    # Test 2: CliError-Isolation + Wallclock-Budget
    # monotonic wird vor Discovery (run_started), je Discovery-Pane (p1, p2) und je Kandidat zweimal geprüft:
    # start=0.0, disc p1=1.0, disc p2=2.0, p1 plan=10.0, p1 execute=20.0 (ok), p2 plan=30.0 (ok), p2 execute=130.0 (> 120s Budget).
    ticks = iter([0.0, 1.0, 2.0, 10.0, 20.0, 30.0, 130.0])
    monkeypatch.setattr(la.time, "monotonic", lambda: next(ticks))

    def fail_execute(adapter, plan, herdr, **kw):
        raise la.CliError("pane kaputt", 4)

    monkeypatch.setattr(la, "execute_bounce", fail_execute)
    out_budget = json.loads(
        la.cmd_auto_switch(
            world.env.adapter,
            as_json=True,
            herdr=FakeHerdr([pane(pane_id="w1:p1", conv=_conv_uuid(1)), pane(pane_id="w1:p2", conv=_conv_uuid(2))]),
        )
    )
    assert out_budget["bounces"][0] == {"pane_id": "w1:p1", "pid": 4101, "status": "error", "error": "pane kaputt"}
    assert out_budget["bounce_summary"]["stopped_reason"] == "wallclock_budget_reached"


def _fresh_cache_world(world):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    _write_quotas(
        world.env,
        active="acc2",
        switch_epoch=SWITCH_EPOCH,
        updated_epoch=time.time(),
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )


def test_phase3_starts_no_bounce_when_budget_below_quiet_recheck_plus_margin(world, monkeypatch):
    """CodeRabbit MAJOR #3: Budget < quiet_recheck + 15 s bedeutet kein execute_bounce und kein /exit."""
    _fresh_cache_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    executed = []
    monkeypatch.setattr(
        la, "execute_bounce",
        lambda adapter, plan, herdr, **kw: executed.append(plan["pane_id"])
        or {**plan, "status": "ok", "new_pid": 9001, "stage": "start", "wakeup_status": "disabled"},
    )
    # run_started=0.0, disc p1=1.0, p1 plan=10.0 (ok), p1 execute=20.0 -> Restbudget 100 s < 300 s Ruhe + 15 s Frist.
    ticks = iter([0.0, 1.0, 10.0, 20.0])
    monkeypatch.setattr(la.time, "monotonic", lambda: next(ticks))

    out = json.loads(la.cmd_auto_switch(
        world.env.adapter, as_json=True, quiet_recheck=300.0,
        herdr=FakeHerdr([pane(pane_id="w1:p1", conv=_conv_uuid(1))]),
    ))

    assert executed == []
    assert out["bounces"] == []
    assert out["bounce_summary"]["executed"] == 0
    assert out["bounce_summary"]["stopped_reason"] == "wallclock_budget_reached"


def test_phase3_stops_planning_once_wallclock_deadline_expired(world, monkeypatch):
    """CodeRabbit MAJOR #3: Die Deadline wird vor plan_bounce geprüft; p2 wird nicht mehr geplant."""
    _fresh_cache_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), start_epoch=BTIME + 200, cwd=world.cwd)
    executed = []
    monkeypatch.setattr(
        la, "execute_bounce",
        lambda adapter, plan, herdr, **kw: executed.append(plan["pane_id"])
        or {**plan, "status": "ok", "new_pid": 9001, "stage": "start", "wakeup_status": "disabled"},
    )
    # run_started=0.0, disc p1=1.0, disc p2=2.0, p1 plan=10.0, p1 execute=20.0 (Rest 100 s >= 45 s), p2 plan=130.0 -> Deadline vorbei.
    ticks = iter([0.0, 1.0, 2.0, 10.0, 20.0, 130.0])
    monkeypatch.setattr(la.time, "monotonic", lambda: next(ticks))

    out = json.loads(la.cmd_auto_switch(
        world.env.adapter, as_json=True,
        herdr=FakeHerdr([pane(pane_id="w1:p1", conv=_conv_uuid(1)), pane(pane_id="w1:p2", conv=_conv_uuid(2))]),
    ))

    assert executed == ["w1:p1"]
    assert [b["pane_id"] for b in out["bounces"] if b["status"] == "ok"] == ["w1:p1"]
    assert out["bounce_summary"]["stopped_reason"] == "wallclock_budget_reached"


def test_phase3_reports_live_blockers_as_skipped_without_execute(world, monkeypatch):
    """Red S3-2: Live-Blocker werden als skipped ausgewiesen statt execute_bounce werfen zu lassen."""
    _fresh_cache_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    executed = []
    monkeypatch.setattr(
        la, "execute_bounce",
        lambda adapter, plan, herdr, **kw: executed.append(plan["pane_id"])
        or {**plan, "status": "ok", "new_pid": 9001, "stage": "start", "wakeup_status": "disabled"},
    )

    out = json.loads(la.cmd_auto_switch(
        world.env.adapter, as_json=True,
        herdr=FakeHerdr([pane(pane_id="w1:p1", conv=_conv_uuid(1), status="working")]),
    ))

    assert executed == []
    entry = out["bounces"][0]
    assert entry["pane_id"] == "w1:p1"
    assert entry["status"] == "skipped"
    assert entry["reason"] == "live_blockers"
    assert entry["blockers"]
    assert out["bounce_summary"]["executed"] == 0
    assert out["bounce_summary"]["stopped_reason"] is None


def test_phase3_does_not_call_herdr_when_no_stale_panes(world, monkeypatch):
    block = _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    _write_quotas(
        world.env,
        active="acc2",
        switch_epoch=block["switch_epoch"],
        updated_epoch=time.time(),
        accounts=[
            _acc_entry("acc1", weekly_pct=90, five_hour_pct=90, weekly_reset=RESET_LATER, is_active=False),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True),
        ],
    )
    # Nur frischer agy-Prozess nach switch_epoch
    add_agy(world.proc, 4101, "w1:p1", start_epoch=SWITCH_EPOCH + 10, cwd=world.cwd)
    called = []
    monkeypatch.setattr(la, "HerdrCli", lambda *a, **k: called.append(True))

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True))

    assert out["bounces"] == []
    assert out["bounce_summary"]["stale_count"] == 0
    assert called == []


# ---------------------------------------------------------------------------
# 7. --reason quota_reached
# ---------------------------------------------------------------------------


def _healthy_quota_world(world):
    write_account(world.env.live, "a@example.invalid")
    world.env.state.mkdir(parents=True, exist_ok=True)
    (world.env.state / "accounts.json").write_text(
        json.dumps({
            "version": 1,
            "providers": {
                "agy": {
                    "active_account": "a",
                    "switch_epoch": SWITCH_EPOCH,
                    "slots": [{"id": "a", "email": "a@example.invalid", "state": "active"}],
                }
            },
        }),
        encoding="utf-8",
    )
    _write_quotas(
        world.env,
        active="a",
        switch_epoch=SWITCH_EPOCH,
        updated_epoch=time.time(),
        accounts=[_acc_entry("a", weekly_pct=80, five_hour_pct=80, weekly_reset=RESET_EARLIER, is_active=True)],
    )


def test_quota_reached_allows_stale_architekt_only_with_evidence_and_source(world):
    _healthy_quota_world(world)
    add_agy(world.proc, 4242, "w1:p1", start_epoch=SWITCH_EPOCH - 50, cwd=world.cwd)
    quota_screen = EMPTY_SCREEN + "Error: Individual quota reached for Gemini\n"

    # Mit Beleg + source + stale -> erlaubt
    plan_ok = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([pane()], tab="architekt", screen=quota_screen),
        proc=world.proc,
        reason="quota_reached",
        source="dv2:msg-9",
    )
    assert plan_ok["blockers"] == []
    assert plan_ok["recommended_action"] == "in_place_resume"

    # Ohne source -> zielbindung_quelle_fehlt
    plan_no_src = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([pane()], tab="architekt", screen=quota_screen),
        proc=world.proc,
        reason="quota_reached",
    )
    assert "zielbindung_quelle_fehlt" in plan_no_src["blockers"]

    # Ohne evidence -> quota_reached_nicht_belegt
    plan_no_ev = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([pane()], tab="architekt", screen=EMPTY_SCREEN),
        proc=world.proc,
        reason="quota_reached",
        source="dv2:msg-9",
    )
    assert "quota_reached_nicht_belegt" in plan_no_ev["blockers"]

    # Menschlicher Tab & fokussierter Tab bleiben strikt gesperrt
    plan_human = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([pane()], tab="vermarkter", screen=quota_screen),
        proc=world.proc,
        reason="quota_reached",
        source="dv2:msg-9",
    )
    assert "protected_human_tab" in plan_human["blockers"]

    plan_focus = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([{**pane(), "focused": True}], tab="architekt", screen=quota_screen),
        proc=world.proc,
        reason="quota_reached",
        source="dv2:msg-9",
    )
    assert "pane_focused" in plan_focus["blockers"]


def test_quota_reached_requires_stale_without_force_pane_and_skips_quiet_recheck(world, monkeypatch):
    _healthy_quota_world(world)
    quota_screen = EMPTY_SCREEN + "Individual quota reached\n"

    # Nicht veraltet und ohne --force-pane -> Exit 4 (schützt vor leerem Pool ohne Wechsel)
    add_agy(world.proc, 4242, "w1:p1", start_epoch=SWITCH_EPOCH + 100, cwd=world.cwd)
    with pytest.raises(la.CliError) as exc:
        la.plan_bounce(
            world.env.adapter,
            "w1:p1",
            FakeHerdr([pane()], tab="architekt", screen=quota_screen),
            proc=world.proc,
            reason="quota_reached",
            source="dv2:msg-10",
        )
    assert exc.value.exit_code == 4

    # Veraltet -> execute_bounce überspringt quiet_recheck_s (kein 30-s-Sleep)
    (world.proc / "4242" / "stat").write_text(
        "4242 (agy) " + " ".join(["S"] + ["0"] * 18 + [str((SWITCH_EPOCH - 50 - BTIME) * TICKS)] + ["0"] * 5) + "\n",
        encoding="utf-8",
    )
    plan = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        FakeHerdr([pane()], tab="architekt", screen=quota_screen),
        proc=world.proc,
        reason="quota_reached",
        source="dv2:msg-10",
    )
    sleeps = []
    monkeypatch.setattr(la.time, "sleep", lambda s: sleeps.append(s))
    monkeypatch.setattr(
        la,
        "_bounce_steps",
        lambda *a, **k: {**plan, "status": "ok", "stage": "start", "heal": "current", "new_pid": 5555},
    )
    res = la.execute_bounce(
        world.env.adapter,
        plan,
        FakeHerdr([pane()], tab="architekt", screen=quota_screen),
        proc=world.proc,
        resume_prompt=False,
        quiet_recheck_s=30.0,
    )
    assert res["status"] == "ok"
    assert 30.0 not in sleeps


# ---------------------------------------------------------------------------
# 8. install-autoswitch-timer
# ---------------------------------------------------------------------------


def test_install_autoswitch_timer_units_no_systemctl_and_preserves_heal_timer(env, tmp_path, monkeypatch):
    systemctl_calls = []
    monkeypatch.setattr(la, "_systemctl_user", lambda args, *, check=True: systemctl_calls.append(args))
    unit_dir = tmp_path / "systemd"
    unit_dir.mkdir(parents=True)
    # Vorbestehende heal-Units dürfen nicht angefasst werden
    heal_service = unit_dir / "llm-account-agy-heal.service"
    heal_timer = unit_dir / "llm-account-agy-heal.timer"
    heal_service.write_text("HEAL-SERVICE-SENTINEL\n", encoding="utf-8")
    heal_timer.write_text("HEAL-TIMER-SENTINEL\n", encoding="utf-8")

    out = json.loads(
        la.cmd_install_autoswitch_timer(
            env.adapter,
            interval=60,
            max_quota_age=180,
            cooldown=300,
            max_bounces=2,
            unit_dir=str(unit_dir),
            activate=False,
            as_json=True,
        )
    )

    assert systemctl_calls == []
    assert out["activated"] is False
    timer_text = (unit_dir / "llm-account-agy-autoswitch.timer").read_text(encoding="utf-8")
    service_text = (unit_dir / "llm-account-agy-autoswitch.service").read_text(encoding="utf-8")

    assert "Path" not in timer_text
    assert "OnBootSec=30s" in timer_text
    assert "OnActiveSec=30s" in timer_text
    assert "OnUnitActiveSec=60s" in timer_text
    assert "AccuracySec=5s" in timer_text
    assert "WantedBy=timers.target" in timer_text

    assert "Slice=herdr-control.slice" in service_text
    assert "TimeoutStartSec=300" in service_text
    assert (
        "ExecStart=%h/.local/bin/llm-account auto-switch agy --max-quota-age 180 --cooldown 300 --max-bounces 2"
        in service_text
    )
    assert oct((unit_dir / "llm-account-agy-autoswitch.service").stat().st_mode & 0o777) == "0o644"
    link = unit_dir / "timers.target.wants" / "llm-account-agy-autoswitch.timer"
    assert link.is_symlink() and os.readlink(link) == "../llm-account-agy-autoswitch.timer"
    assert heal_service.read_text(encoding="utf-8") == "HEAL-SERVICE-SENTINEL\n"
    assert heal_timer.read_text(encoding="utf-8") == "HEAL-TIMER-SENTINEL\n"


@pytest.mark.parametrize("argv", [
    ["install-autoswitch-timer", "agy", "--interval", "14"],
    ["install-autoswitch-timer", "agy", "--interval", "3601"],
    ["install-autoswitch-timer", "agy", "--max-quota-age", "nan"],
    ["install-autoswitch-timer", "agy", "--cooldown", "-1"],
    ["install-autoswitch-timer", "agy", "--max-bounces", "11"],
])
def test_install_autoswitch_timer_invalid_args_exit_2_without_writing(env, tmp_path, argv):
    unit_dir = tmp_path / "systemd-invalid"
    assert la.main([*argv, "--systemd-dir", str(unit_dir), "--no-systemctl"]) == 2
    assert not unit_dir.exists()


# ---------------------------------------------------------------------------
# 9. 0600-Sicherheit & O_NOFOLLOW/FIFO gegen ersetzte State-Dateien
# ---------------------------------------------------------------------------


def test_fifo_quotas_json_is_discarded_fail_closed_without_blocking(env):
    """CodeRabbit MAJOR #2: Eine FIFO statt quotas.json blockiert das Lesen nicht und gilt als unlesbar."""
    _setup_two_slots(env, active="acc2", switch_epoch=SWITCH_EPOCH)
    env.state.mkdir(parents=True, exist_ok=True)
    os.mkfifo(env.state / "quotas.json")

    assert la._read_quotas_snapshot_safe() is None
    assert "disk_token_unhealthy:quotas_unlesbar" in la.disk_token_blockers(env.adapter)
    # Ohne Writer bleibt die FIFO liegen; der Leseaufruf ist trotzdem sofort zurückgekehrt.
    assert (env.state / "quotas.json").exists()


@pytest.mark.parametrize("corrupt_accounts", [
    "accounts_keine_liste",
    "account_kein_dict",
    "id_kein_str",
    "lane_kein_dict",
])
def test_corrupt_quota_snapshot_structure_fails_closed_as_cache_miss(env, corrupt_accounts):
    """Struktur-Validierung: kaputte Snapshots sind quotas_unlesbar bzw. Cache-Miss statt AttributeError."""
    block = _setup_two_slots(env, active="acc2", switch_epoch=SWITCH_EPOCH)
    good_account = _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=True)
    if corrupt_accounts == "accounts_keine_liste":
        accounts = {"acc2": good_account}
    elif corrupt_accounts == "account_kein_dict":
        accounts = ["acc2"]
    elif corrupt_accounts == "id_kein_str":
        accounts = [{**good_account, "id": 42}]
    else:
        accounts = [{**good_account, "lanes": {"gemini": None}}]
    snap = {
        "version": 1,
        "updated_epoch": time.time(),
        "providers": {"agy": {
            "active_account": "acc2", "switch_epoch": block["switch_epoch"], "accounts": accounts,
        }},
    }
    reg_block = la.provider_block(la.load_registry(), env.adapter)

    assert la._cache_snapshot_matches_block(snap, env.adapter, reg_block, time.time(), 180.0)[0] is False

    _write_quotas(
        env, active="acc2", switch_epoch=block["switch_epoch"], updated_epoch=time.time(), accounts=accounts,
    )
    assert "disk_token_unhealthy:quotas_unlesbar" in la.disk_token_blockers(env.adapter)


def test_symlink_quotas_json_and_symlink_lockfile_are_rejected_fail_closed(env, tmp_path, monkeypatch):
    block = _setup_two_slots(env, active="acc2", switch_epoch=SWITCH_EPOCH)
    poisoned = tmp_path / "poisoned.json"
    poisoned.write_text(
        json.dumps({
            "version": 1,
            "updated_epoch": time.time(),
            "providers": {"agy": {"active_account": "acc2", "switch_epoch": block["switch_epoch"], "accounts": []}},
        }),
        encoding="utf-8",
    )
    (env.state / "quotas.json").symlink_to(poisoned)

    assert la._read_quotas_snapshot_safe() is None
    assert "disk_token_unhealthy:quotas_unlesbar" in la.disk_token_blockers(env.adapter)

    # auto-switch ignoriert das Symlink-quotas.json und probt sauber live statt dem Symlink zu trauen
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 50, 50, 50, resetw=RESET_EARLIER),
    })
    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))
    assert out["probe_used"] is True
    assert not (env.state / "quotas.json").is_symlink()  # atomar ersetzt
    assert oct((env.state / "quotas.json").stat().st_mode & 0o777) == "0o600"
    assert oct(env.state.stat().st_mode & 0o777) == "0o700"

    # Symlink-Lockfile für auto-switch-bounce.lock wird über O_NOFOLLOW abgewiesen
    proc = tmp_path / "proc"
    proc.mkdir()
    (proc / "stat").write_text(f"cpu 1 2 3\nbtime {BTIME}\n", encoding="utf-8")
    monkeypatch.setenv("LLM_ACCOUNTS_PROC", str(proc))
    add_agy(proc, 4242, "w1:p1", start_epoch=BTIME + 500, cwd=str(tmp_path))
    target_file = tmp_path / "decoy-lock"
    target_file.write_text("x", encoding="utf-8")
    (env.state / "auto-switch-bounce.lock").symlink_to(target_file)

    with pytest.raises(la.CliError) as exc:
        la.cmd_auto_switch(env.adapter, as_json=True, herdr=FakeHerdr([pane()]))
    assert exc.value.exit_code == 3


# ---------------------------------------------------------------------------
# 10. Delta-Protokollierung (agy-account-switch-deltas.jsonl)
# ---------------------------------------------------------------------------


def test_auto_switch_writes_delta_jsonl_after_post_bounce_reprobe_without_secrets(world, tmp_path, monkeypatch):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    set_usage(world.env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 2, 50, 2, resetw=RESET_EARLIER),
    })
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    deltas_path = tmp_path / "mothr-deltas" / "agy-account-switch-deltas.jsonl"
    monkeypatch.setenv("MOTHR_AGY_SWITCH_DELTAS_FILE", str(deltas_path))

    class DeltaHerdr(FakeHerdr):
        def tab_label(self, tab_id):
            return "bauer"

        def run(self, pane_id, text):
            super().run(pane_id, text)
            if text == "/exit":
                for sub in (world.proc / "4101").iterdir():
                    sub.unlink()
                (world.proc / "4101").rmdir()
            elif "agy --conversation" in text:
                add_agy(world.proc, 4999, pane_id, conv=_conv_uuid(1), start_epoch=int(time.time()), cwd=world.cwd)
                # Alt-Konto hat während des Bounces weiter Quota verbraucht (2% -> 1% weekly, 50% -> 46% 5h).
                set_usage(world.env, {
                    "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
                    "b@example.test": tsv(46, 1, 46, 1, resetw=RESET_EARLIER),
                })

    herdr = DeltaHerdr([{**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t-b", "focused": False}])
    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))

    assert out["decision"] == "switched"
    assert out["bounce_summary"]["executed"] == 1
    assert deltas_path.exists()
    assert oct(deltas_path.parent.stat().st_mode & 0o777) == "0o700"
    assert oct(deltas_path.stat().st_mode & 0o777) == "0o600"

    raw = deltas_path.read_text(encoding="utf-8")
    lines = [line for line in raw.splitlines() if line.strip()]
    assert len(lines) == 1
    assert "SECRET-" not in raw
    assert "a@example.test" not in raw
    assert "b@example.test" not in raw
    assert "oauth" not in raw.lower()
    assert "refresh_token" not in raw
    assert "access_token" not in raw

    entry = json.loads(lines[0])
    assert entry["schema"] == "agy-account-switch-delta-v1"
    assert entry["provider"] == "agy"
    assert entry["from_slot"] == "acc2"
    assert entry["to_slot"] == "acc1"
    assert entry["reason"] == "weekly_exhausted"
    assert entry["threshold_pct"] == 2
    assert entry["old_weekly_pct_before"] == 2
    assert entry["old_five_hour_pct_before"] == 50
    assert entry["old_weekly_pct_after"] == 1
    assert entry["old_five_hour_pct_after"] == 46
    assert entry["weekly_delta_pct"] == 1
    assert entry["five_hour_delta_pct"] == 4
    assert entry["stale_sessions_bounced"] == 1
    assert entry["total_duration_seconds"] >= 0.0
    assert entry["post_bounce_measured"] is True
    assert entry["ts_utc"]
    assert entry["started_at_utc"]
    assert entry["completed_at_utc"]


def test_dry_run_never_writes_delta_jsonl(env, tmp_path, monkeypatch):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 2, 50, 2, resetw=RESET_EARLIER),
    })
    deltas_path = tmp_path / "mothr-dry" / "agy-account-switch-deltas.jsonl"
    monkeypatch.setenv("MOTHR_AGY_SWITCH_DELTAS_FILE", str(deltas_path))

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, dry_run=True, no_bounce=True))

    assert out["decision"] == "dry_run_would_switch"
    assert not deltas_path.exists()


def test_delta_log_write_error_does_not_fail_auto_switch(env, tmp_path, monkeypatch):
    _setup_two_slots(env, active="acc2", switch_epoch=time.time() - 600)
    set_usage(env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 2, 50, 2, resetw=RESET_EARLIER),
    })
    decoy = tmp_path / "decoy.jsonl"
    decoy.write_text("do-not-touch\n", encoding="utf-8")
    symlink_deltas = tmp_path / "symlink-deltas.jsonl"
    symlink_deltas.symlink_to(decoy)
    monkeypatch.setenv("MOTHR_AGY_SWITCH_DELTAS_FILE", str(symlink_deltas))

    out = json.loads(la.cmd_auto_switch(env.adapter, as_json=True, no_bounce=True))

    assert out["decision"] == "switched"
    assert out["active_account"] == "acc1"
    assert _read_registry_block(env)["active_account"] == "acc1"
    assert decoy.read_text(encoding="utf-8") == "do-not-touch\n"


def test_failed_bounce_is_not_counted_in_stale_sessions_bounced(world, tmp_path, monkeypatch):
    _setup_two_slots(world.env, active="acc2", switch_epoch=SWITCH_EPOCH)
    set_usage(world.env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 2, 50, 2, resetw=RESET_EARLIER),
    })
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    deltas_path = tmp_path / "mothr-failed-bounce" / "agy-account-switch-deltas.jsonl"
    monkeypatch.setenv("MOTHR_AGY_SWITCH_DELTAS_FILE", str(deltas_path))
    monkeypatch.setattr(
        la,
        "execute_bounce",
        lambda adapter, plan, herdr, **kw: {**plan, "status": "failed", "stage": "exit"},
    )

    class TabHerdr(FakeHerdr):
        def tab_label(self, tab_id):
            return "bauer"

    herdr = TabHerdr([{**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t-b", "focused": False}])
    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))

    assert out["decision"] == "switched"
    assert out["bounce_summary"]["executed"] == 1
    assert out["bounces"][0]["status"] == "failed"

    entry = json.loads(deltas_path.read_text(encoding="utf-8").splitlines()[0])
    assert entry["stale_sessions_bounced"] == 0


def test_append_jsonl_locked_truncates_partial_write_on_error(tmp_path, monkeypatch):
    deltas_path = tmp_path / "mothr-partial" / "agy-account-switch-deltas.jsonl"
    la._append_jsonl_locked(deltas_path, {"seq": 1})
    baseline = deltas_path.read_text(encoding="utf-8")

    real_write = os.write
    calls = 0

    def flaky_write(fd, data):
        nonlocal calls
        calls += 1
        if calls == 1:
            return real_write(fd, bytes(data[:5]))
        raise OSError(28, "No space left on device")

    monkeypatch.setattr(la.os, "write", flaky_write)
    with pytest.raises(OSError):
        la._append_jsonl_locked(deltas_path, {"seq": 2})

    assert deltas_path.read_text(encoding="utf-8") == baseline

    monkeypatch.setattr(la.os, "write", real_write)
    la._append_jsonl_locked(deltas_path, {"seq": 3})
    parsed = [json.loads(line) for line in deltas_path.read_text(encoding="utf-8").splitlines()]
    assert parsed == [{"seq": 1}, {"seq": 3}]


# ---------------------------------------------------------------------------
# 11. P0 Live-Rebind (iq-acd838c44a3d)
# ---------------------------------------------------------------------------


def _p0_switch_world(world, *, now_epoch: float | None = None):
    now = time.time() if now_epoch is None else now_epoch
    _setup_two_slots(world.env, active="acc2", switch_epoch=now - 600)
    set_usage(world.env, {
        "a@example.test": tsv(90, 90, 90, 90, resetw=RESET_LATER),
        "b@example.test": tsv(50, 1, 50, 1, resetw=RESET_EARLIER),
    })


def _make_p0_herdr(world, panes, labels, *, screens=None, exit_pids=None, new_pid_base=4900):
    import shutil

    screens = screens or {}
    exit_pids = exit_pids or {}
    counter = {"n": 0}

    class P0Herdr(FakeHerdr):
        def tab_label(self, tab_id):
            return labels.get(tab_id, self.tab)

        def screen_text(self, pane_id):
            return screens.get(pane_id, self.screen)

        def run(self, pane_id, text):
            super().run(pane_id, text)
            if text == "/exit":
                pid = exit_pids.get(pane_id)
                if pid is not None and (world.proc / str(pid)).exists():
                    shutil.rmtree(world.proc / str(pid))
            elif "agy --conversation" in text:
                counter["n"] += 1
                conv = next((p.get("agent_session", {}).get("value") for p in self.panes if p["pane_id"] == pane_id), None)
                add_agy(
                    world.proc,
                    new_pid_base + counter["n"],
                    pane_id,
                    conv=conv,
                    model=None,
                    start_epoch=int(time.time()),
                    cwd=world.cwd,
                )

    return P0Herdr(panes)


def test_p0_no_model_agy_process_rebinds_without_model_and_rejects_invalid_model(world):
    import shlex

    _p0_switch_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), model=None, start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), model="gemini-4;rm -rf /", start_epoch=BTIME + 200, cwd=world.cwd)

    panes = [
        {**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t1", "focused": False},
        {**pane(pane_id="w1:p2", conv=_conv_uuid(2), status="idle"), "tab_id": "t2", "focused": False},
    ]
    herdr = _make_p0_herdr(world, panes, {"t1": "Nr1", "t2": "bauer"}, exit_pids={"w1:p1": 4101})

    # Direkter plan_bounce mit allow_default_model=True baut Kommando ohne --model
    direct_plan = la.plan_bounce(
        world.env.adapter, "w1:p1", herdr, proc=world.proc, allow_default_model=True
    )
    assert direct_plan["blockers"] == []
    assert direct_plan["model"] is None
    assert direct_plan["resume_cmd"] == ["agy", "--conversation", _conv_uuid(1)]

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))

    assert out["decision"] == "switched"
    by_pane = {b["pane_id"]: b for b in out["bounces"]}
    assert by_pane["w1:p1"]["status"] == "ok"
    restart_cmds = [c[1] for c in herdr.calls if c[0] == "w1:p1" and c[1].startswith("cd ")]
    assert len(restart_cmds) == 1
    assert shlex.split(restart_cmds[0]) == ["cd", world.cwd, "&&", "agy", "--conversation", _conv_uuid(1)]

    assert by_pane["w1:p2"]["status"] == "skipped"
    assert by_pane["w1:p2"]["recommended_action"] == "herdr-mutter-kind"
    assert any("unzulässige Zeichen" in b for b in by_pane["w1:p2"]["blockers"])


def test_p0_large_db_above_soft_threshold_bounces_with_warning_and_blocks_above_hard_cap(world, monkeypatch):
    assert la.BOUNCE_HARD_MAX_BYTES == 1024 * 1024 * 1024
    _p0_switch_world(world)
    conv_dir = world.env.live / ".gemini/antigravity-cli/conversations"
    (conv_dir / f"{_conv_uuid(1)}.pb").write_bytes(b"a" * 2000)
    (conv_dir / f"{_conv_uuid(2)}.pb").write_bytes(b"b" * 6000)
    monkeypatch.setattr(la, "BOUNCE_MAX_BYTES", 1000)
    monkeypatch.setattr(la, "BOUNCE_HARD_MAX_BYTES", 5000)

    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), model=None, start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), model=None, start_epoch=BTIME + 200, cwd=world.cwd)

    panes = [
        {**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t1", "focused": False},
        {**pane(pane_id="w1:p2", conv=_conv_uuid(2), status="idle"), "tab_id": "t2", "focused": False},
    ]
    herdr = _make_p0_herdr(world, panes, {"t1": "Nr1", "t2": "bauer"}, exit_pids={"w1:p1": 4101})

    # Ohne allow_large_db bleibt conv1 blockiert
    plan_strict = la.plan_bounce(
        world.env.adapter, "w1:p1", herdr, proc=world.proc, allow_default_model=True
    )
    assert plan_strict["large_db_warning"] is True
    assert any("Schwelle" in b for b in plan_strict["blockers"])

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    by_pane = {b["pane_id"]: b for b in out["bounces"]}
    assert by_pane["w1:p1"]["status"] == "ok"
    assert by_pane["w1:p1"]["large_db_warning"] is True
    assert by_pane["w1:p2"]["status"] == "skipped"
    assert by_pane["w1:p2"]["recommended_action"] == "herdr-mutter-kind"
    assert any("Schwelle" in b for b in by_pane["w1:p2"]["blockers"])


def test_p0_phase3_bounces_idle_architekt_tab_while_keeping_human_tabs_protected(world):
    _p0_switch_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), model=MODEL, start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), model=MODEL, start_epoch=BTIME + 200, cwd=world.cwd)
    add_agy(world.proc, 4103, "w1:p3", conv=_conv_uuid(3), model=MODEL, start_epoch=BTIME + 300, cwd=world.cwd)

    panes = [
        {**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t-arch", "focused": False},
        {**pane(pane_id="w1:p2", conv=_conv_uuid(2), status="idle"), "tab_id": "t-verm", "focused": False},
        {**pane(pane_id="w1:p3", conv=_conv_uuid(3), status="idle"), "tab_id": "t-ds", "focused": False},
    ]
    labels = {"t-arch": "🏗️ architekt-2", "t-verm": "vermarkter", "t-ds": "design studio"}
    herdr = _make_p0_herdr(world, panes, labels, exit_pids={"w1:p1": 4101})

    # Direkter plan_bounce ohne allow_architekt schützt weiter
    assert "protected_architekt_tab" in la.plan_bounce(
        world.env.adapter, "w1:p1", herdr, proc=world.proc
    )["blockers"]

    out = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    by_pane = {b["pane_id"]: b for b in out["bounces"]}
    assert by_pane["w1:p1"]["status"] == "ok"
    assert by_pane["w1:p2"]["status"] == "skipped"
    assert "protected_human_tab" in by_pane["w1:p2"]["blockers"]
    assert by_pane["w1:p3"]["status"] == "skipped"
    assert "protected_human_tab" in by_pane["w1:p3"]["blockers"]


def test_p0_quota_reached_upgrades_priority_skips_30s_sleep_and_wakes_session(world, fake_chat, monkeypatch):
    _p0_switch_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), model=MODEL, start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), model=MODEL, start_epoch=BTIME + 200, cwd=world.cwd)

    panes = [
        {**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t1", "focused": False},
        {**pane(pane_id="w1:p2", conv=_conv_uuid(2), status="idle"), "tab_id": "t2", "focused": False},
    ]
    quota_screen = EMPTY_SCREEN + "Individual quota reached. Please upgrade your subscription.\n"
    herdr = _make_p0_herdr(
        world,
        panes,
        {"t1": "bauer", "t2": "🏗️ architekt-2"},
        screens={"w1:p1": EMPTY_SCREEN, "w1:p2": quota_screen},
        exit_pids={"w1:p2": 4102},
    )

    sleeps = []
    real_sleep = time.sleep
    monkeypatch.setattr(la.time, "sleep", lambda s: sleeps.append(s) or real_sleep(min(s, 0.005)))

    out = json.loads(
        la.cmd_auto_switch(world.env.adapter, as_json=True, max_bounces=1, quiet_recheck=30.0, herdr=herdr)
    )

    assert out["bounce_summary"]["executed"] == 1
    assert out["bounce_summary"]["stopped_reason"] == "max_bounces_reached"
    assert len(out["bounces"]) == 1
    entry = out["bounces"][0]
    assert entry["pane_id"] == "w1:p2"
    assert entry["status"] == "ok"
    assert entry["wakeup_status"] == "working_seen"
    assert 30.0 not in sleeps


def test_p0_focused_idle_pane_settles_after_60s_resets_on_draft_or_working_and_rescues_quota(world, fake_chat, monkeypatch):
    assert la.FOCUSED_IDLE_SETTLE_S == 60.0
    base_now = 1_800_000_000.0
    clock = [base_now]
    monkeypatch.setattr(la.time, "time", lambda: clock[0])

    _p0_switch_world(world, now_epoch=base_now)
    # Cache frisch halten, damit nachfolgende Ticks ohne Live-Probe sauber Phase 3 durchlaufen
    _write_quotas(
        world.env,
        active="acc1",
        switch_epoch=base_now,
        updated_epoch=base_now,
        accounts=[
            _acc_entry("acc1", weekly_pct=95, five_hour_pct=95, weekly_reset=RESET_LATER, is_active=True),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=False),
        ],
    )
    reg_path = world.env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["active_account"] = "acc1"
    reg["providers"]["agy"]["switch_epoch"] = base_now
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    write_account(world.env.live, "a@example.test")

    add_agy(world.proc, 4101, "w1NA:pQ", conv=_conv_uuid(1), model=None, start_epoch=BTIME + 100, cwd=world.cwd)
    panes = [{**pane(pane_id="w1NA:pQ", conv=_conv_uuid(1), status="idle"), "tab_id": "t-nr1", "focused": True}]
    screens = {"w1NA:pQ": EMPTY_SCREEN}
    herdr = _make_p0_herdr(world, panes, {"t-nr1": "Nr1"}, screens=screens, exit_pids={"w1NA:pQ": 4101})

    def _tick():
        # Cache frisch halten (updated_epoch == clock[0])
        q_path = world.env.state / "quotas.json"
        q = json.loads(q_path.read_text(encoding="utf-8"))
        q["updated_epoch"] = clock[0]
        q_path.write_text(json.dumps(q), encoding="utf-8")
        return json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))

    # Vorbestehenden Anti-Loop-Eintrag auf einer anderen Pane anlegen -> darf nicht gelöscht werden
    la.record_bounce_attempt("w9:other", base_now - 100, now=base_now - 10)

    # Tick 1 (t=0): focused idle -> noch nicht settled, startet Timer in _focused_idle
    out1 = _tick()
    assert out1["bounce_summary"]["executed"] == 0
    assert out1["bounces"][0]["status"] == "skipped"
    state1 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert "w9:other" in state1
    assert "w1NA:pQ" in state1["_focused_idle"]
    assert oct((world.env.state / "bounce-state.json").stat().st_mode & 0o777) == "0o600"

    # Tick 2 (t=+30s): noch < 60s -> weiter geskippt
    clock[0] = base_now + 30.0
    out2 = _tick()
    assert out2["bounce_summary"]["executed"] == 0

    # Tick 3 (t=+61s, aber Composer-Entwurf!): NIEMALS bouncen, Settle gelöscht!
    clock[0] = base_now + 61.0
    screens["w1NA:pQ"] = "─" * 40 + "\n> hallo\n" + "─" * 40 + "\nauto · Gemini 4 Neon · high\n"
    out3 = _tick()
    assert out3["bounce_summary"]["executed"] == 0
    assert out3["bounces"][0]["status"] == "skipped"
    state3 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert "w1NA:pQ" not in state3.get("_focused_idle", {})

    # Tick 4 (t=+62s, Composer wieder leer): startet Settle neu
    clock[0] = base_now + 62.0
    screens["w1NA:pQ"] = EMPTY_SCREEN
    out4 = _tick()
    assert out4["bounce_summary"]["executed"] == 0

    # Tick 5 (t=+100s, status=working): NIEMALS bouncen, Settle gelöscht!
    clock[0] = base_now + 100.0
    panes[0]["agent_status"] = "working"
    out5 = _tick()
    assert out5["bounce_summary"]["executed"] == 0
    state5 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert "w1NA:pQ" not in state5.get("_focused_idle", {})

    # Tick 6 (t=+101s, wieder idle): Settle startet ab t=+101s
    clock[0] = base_now + 101.0
    panes[0]["agent_status"] = "idle"
    out6 = _tick()
    assert out6["bounce_summary"]["executed"] == 0

    # Tick 7 (t=+162s, >= 60s unverändert idle + leerer Composer): wird sauber gebounct!
    clock[0] = base_now + 162.0
    out7 = _tick()
    assert out7["bounce_summary"]["executed"] == 1
    assert out7["bounces"][0]["status"] == "ok"
    assert out7["bounces"][0]["wakeup_status"] == "disabled"
    state7 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert "w9:other" in state7
    assert "w1NA:pQ" not in state7.get("_focused_idle", {})

    # Gegenprobe: Focused Pane mit "Individual quota reached" + leerem Composer wird SOFORT gerettet!
    add_agy(world.proc, 4201, "w1:pQ2", conv=_conv_uuid(2), model=None, start_epoch=BTIME + 100, cwd=world.cwd)
    panes2 = [{**pane(pane_id="w1:pQ2", conv=_conv_uuid(2), status="idle"), "tab_id": "t-nr2", "focused": True}]
    herdr2 = _make_p0_herdr(
        world,
        panes2,
        {"t-nr2": "Nr1"},
        screens={"w1:pQ2": EMPTY_SCREEN + "Individual quota reached\n"},
        exit_pids={"w1:pQ2": 4201},
        new_pid_base=5000,
    )
    out_quota = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr2))
    assert out_quota["bounce_summary"]["executed"] == 1
    assert out_quota["bounces"][0]["pane_id"] == "w1:pQ2"
    assert out_quota["bounces"][0]["status"] == "ok"
    assert out_quota["bounces"][0]["wakeup_status"] == "working_seen"


def test_p0_focused_idle_settle_race_in_plan_and_fresh_blockers(world, fake_chat, monkeypatch):
    """CodeRabbit MAJOR: Fokus-Settle prüft Screen-Hash gegen plan_bounce und _fresh_blockers und startet bei Wechsel neu."""
    base_now = 1_800_000_000.0
    clock = [base_now]
    monkeypatch.setattr(la.time, "time", lambda: clock[0])

    _p0_switch_world(world, now_epoch=base_now)
    _write_quotas(
        world.env,
        active="acc1",
        switch_epoch=base_now,
        updated_epoch=base_now,
        accounts=[
            _acc_entry("acc1", weekly_pct=95, five_hour_pct=95, weekly_reset=RESET_LATER, is_active=True),
            _acc_entry("acc2", weekly_pct=50, five_hour_pct=50, weekly_reset=RESET_EARLIER, is_active=False),
        ],
    )
    reg_path = world.env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    reg["providers"]["agy"]["active_account"] = "acc1"
    reg["providers"]["agy"]["switch_epoch"] = base_now
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    write_account(world.env.live, "a@example.test")

    screen_a = EMPTY_SCREEN + "idle output A\n"
    screen_b = EMPTY_SCREEN + "idle output B\n"
    sha_a = la._screen_sha256(screen_a)
    sha_b = la._screen_sha256(screen_b)

    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), model=None, start_epoch=BTIME + 100, cwd=world.cwd)
    panes = [{**pane(pane_id="w1:p1", conv=_conv_uuid(1), status="idle"), "tab_id": "t-nr1", "focused": True}]
    screen_seq = [screen_a]

    class SeqHerdr(_make_p0_herdr(world, panes, {"t-nr1": "Nr1"}, exit_pids={"w1:p1": 4101}).__class__):
        def screen_text(self, pane_id):
            if len(screen_seq) > 1:
                return screen_seq.pop(0)
            return screen_seq[0]

    herdr = SeqHerdr(panes)

    def _refresh_quota():
        q_path = world.env.state / "quotas.json"
        q = json.loads(q_path.read_text(encoding="utf-8"))
        q["updated_epoch"] = clock[0]
        q_path.write_text(json.dumps(q), encoding="utf-8")

    # 1) Tick 1 bei t=0 startet Settle auf Screen A
    out1 = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    assert out1["bounce_summary"]["executed"] == 0
    state1 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert state1["_focused_idle"]["w1:p1"]["screen_sha256"] == sha_a

    # 2) Nach 65s: direkter plan_bounce mit expected_focused_screen_sha256=sha_a sieht Screen B -> blockiert
    clock[0] = base_now + 65.0
    _refresh_quota()
    screen_seq[:] = [screen_b]
    direct_plan = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        herdr,
        proc=world.proc,
        allow_focused=True,
        expected_focused_screen_sha256=sha_a,
        allow_default_model=True,
    )
    assert "focused_screen_changed" in direct_plan["blockers"]
    assert direct_plan["recommended_action"] == "herdr-mutter-kind"
    assert direct_plan["screen_sha256"] == sha_b
    assert direct_plan["expected_focused_screen_sha256"] == sha_a

    # 3) Phase-3-Race: Discovery sieht gesettelten Screen A, plan_bounce sieht Screen B -> kein /exit, Settle auf B neu gestartet
    screen_seq[:] = [screen_a, screen_b]
    out_plan_race = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    assert out_plan_race["bounce_summary"]["executed"] == 0
    assert out_plan_race["bounces"][0]["status"] == "skipped"
    assert out_plan_race["bounces"][0]["detail"] == "focused_idle_settling"
    assert "focused_screen_changed" in out_plan_race["bounces"][0]["blockers"]
    assert not any(c[1] == "/exit" for c in herdr.calls)
    state_b1 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert state_b1["_focused_idle"]["w1:p1"]["screen_sha256"] == sha_b

    # 4) Settle auf Screen B nach weiteren 65s abgeschlossen; danach Race erst vor /exit in _fresh_blockers
    clock[0] = base_now + 130.0
    _refresh_quota()
    # Discovery = B, plan_bounce = B (eligible), _fresh_blockers = A (anders!) -> fail-closed ohne /exit, Settle auf A
    screen_seq[:] = [screen_b, screen_b, screen_a]
    out_fresh_race = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    assert out_fresh_race["bounce_summary"]["executed"] == 0
    assert out_fresh_race["bounces"][0] == {
        "pane_id": "w1:p1",
        "pid": 4101,
        "status": "skipped",
        "reason": "focused_screen_changed",
        "detail": "focused_idle_settling",
    }
    assert not any(c[1] == "/exit" for c in herdr.calls)
    state_a2 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert state_a2["_focused_idle"]["w1:p1"]["screen_sha256"] == sha_a

    # Auch direktes execute_bounce bricht bei Hash-Wechsel fail-closed ohne /exit ab und startet Settle neu
    clock[0] = base_now + 195.0
    screen_seq[:] = [screen_a]
    ok_plan = la.plan_bounce(
        world.env.adapter,
        "w1:p1",
        herdr,
        proc=world.proc,
        allow_focused=True,
        expected_focused_screen_sha256=sha_a,
        allow_default_model=True,
    )
    assert ok_plan["recommended_action"] == "in_place_resume"
    screen_seq[:] = [screen_b]
    with pytest.raises(la.CliError, match="focused_screen_changed"):
        la.execute_bounce(world.env.adapter, ok_plan, herdr, proc=world.proc, quiet_recheck_s=0.0)
    assert not any(c[1] == "/exit" for c in herdr.calls)
    state_b2 = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert state_b2["_focused_idle"]["w1:p1"]["screen_sha256"] == sha_b

    # 5) Gegenprobe: Bleibt Screen B nach >= 60s unverändert, bleibt die fokussierte Pane bounce-berechtigt!
    clock[0] = base_now + 260.0
    _refresh_quota()
    screen_seq[:] = [screen_b]
    out_ok = json.loads(la.cmd_auto_switch(world.env.adapter, as_json=True, quiet_recheck=0.0, herdr=herdr))
    assert out_ok["bounce_summary"]["executed"] == 1
    assert out_ok["bounces"][0]["status"] == "ok"
    assert any(c[1] == "/exit" for c in herdr.calls)
    state_done = json.loads((world.env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert "w1:p1" not in state_done.get("_focused_idle", {})


def test_phase3_discovery_stops_immediately_when_wallclock_budget_exhausted(world, monkeypatch):
    """CodeRabbit MINOR: Erschöpft die Discovery-Schleife das 120s-Budget, bricht sie vor weiteren screen_text-Reads ab."""
    _fresh_cache_world(world)
    add_agy(world.proc, 4101, "w1:p1", conv=_conv_uuid(1), start_epoch=BTIME + 100, cwd=world.cwd)
    add_agy(world.proc, 4102, "w1:p2", conv=_conv_uuid(2), start_epoch=BTIME + 200, cwd=world.cwd)
    add_agy(world.proc, 4103, "w1:p3", conv=_conv_uuid(3), start_epoch=BTIME + 300, cwd=world.cwd)

    screen_reads = []
    executed = []
    monkeypatch.setattr(
        la,
        "execute_bounce",
        lambda adapter, plan, herdr, **kw: executed.append(plan["pane_id"])
        or {**plan, "status": "ok", "new_pid": 9001, "stage": "start", "wakeup_status": "disabled"},
    )

    class TrackingHerdr(FakeHerdr):
        def screen_text(self, pane_id):
            screen_reads.append(pane_id)
            return super().screen_text(pane_id)

    # run_started=0.0, disc p1=10.0 (ok), disc p2=125.0 (> 120s Budget -> sofortiger Abbruch vor p2 screen_text)
    ticks = iter([0.0, 10.0, 125.0])
    monkeypatch.setattr(la.time, "monotonic", lambda: next(ticks))

    out = json.loads(
        la.cmd_auto_switch(
            world.env.adapter,
            as_json=True,
            herdr=TrackingHerdr([
                pane(pane_id="w1:p1", conv=_conv_uuid(1)),
                pane(pane_id="w1:p2", conv=_conv_uuid(2)),
                pane(pane_id="w1:p3", conv=_conv_uuid(3)),
            ]),
        )
    )

    assert screen_reads == ["w1:p1"]
    assert executed == []
    assert out["bounces"] == []
    assert out["bounce_summary"]["executed"] == 0
    assert out["bounce_summary"]["stopped_reason"] == "wallclock_budget_reached"
