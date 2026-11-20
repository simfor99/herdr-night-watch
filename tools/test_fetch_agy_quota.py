"""Tests für tools/fetch_agy_quota.py: JSON-Vertrag für src/quota.rs, Fallback und fail-closed-Verhalten."""

import importlib.util
import json
import os
import sys
from pathlib import Path

TOOLS = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, TOOLS)

import test_llm_account as helpers  # noqa: E402
from test_llm_account import env  # noqa: E402,F401  (Fixture wird in diesem Modul gebraucht)

FETCH_PATH = Path(TOOLS) / "fetch_agy_quota.py"


def load_fetch_module():
    spec = importlib.util.spec_from_file_location("fetch_agy_quota_under_test", FETCH_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run_fetch(capsys) -> object:
    fetch = load_fetch_module()
    capsys.readouterr()  # Setup-Ausgaben der Helfer verwerfen
    assert fetch.main() == 0
    out = capsys.readouterr().out.strip()
    return json.loads(out)


def test_registry_mode_emits_the_rust_contract_with_only_active_slots(env, capsys):
    helpers.two_accounts_saved(env)  # aktiv acc2, acc1 ist gesichert
    helpers.set_usage(env, {
        "a@example.test": helpers.tsv(0, 0, 0, 0, reset5="2026-11-23T23:29:00Z", resetw="2026-11-23T23:29:00Z"),
        "b@example.test": helpers.tsv(98, 99, 100, 100),
    })

    doc = run_fetch(capsys)

    assert set(doc) == {"version", "active_account", "pooled_count", "pool_next_refill_epoch", "accounts"}
    assert doc["version"] == 1
    assert doc["active_account"] == "acc2"
    assert doc["pooled_count"] == 2
    assert doc["pool_next_refill_epoch"] == 1795476540  # frühester weekly_reset des anderen Slots: 2026-11-23T23:29:00Z
    by_id = {a["id"]: a for a in doc["accounts"]}
    assert set(by_id) == {"acc1", "acc2"}
    assert by_id["acc2"] == {
        "id": "acc2",
        "label": "work",
        "email": "b@example.test",
        "is_active": True,
        "auth_state": "ok",
        "auth_error": None,
        "five_hour_pct": 98,
        "five_hour_reset": "2026-11-20T13:08:12Z",
        "week_pct": 99,
        "week_reset": "2026-11-27T08:08:12Z",
    }
    assert by_id["acc1"]["is_active"] is False
    assert "SECRET" not in json.dumps(doc)


def test_retired_slots_do_not_appear_as_ghost_cards(env, capsys):
    helpers.two_accounts_saved(env)
    helpers.set_usage(env, {"a@example.test": helpers.tsv(0, 0, 0, 0), "b@example.test": helpers.tsv(50, 60, 70, 80)})
    import llm_account as la

    la.main(["retire", "agy", "acc1"])
    doc = run_fetch(capsys)

    assert [a["id"] for a in doc["accounts"]] == ["acc2"]
    assert doc["pooled_count"] == 1
    assert doc["pool_next_refill_epoch"] is None


def test_without_registry_falls_back_to_one_direct_agy_call_on_the_live_home(env, capsys):
    helpers.write_account(env.live, "live@example.test")
    helpers.set_usage(env, {"live@example.test": helpers.tsv(40, 55, 60, 70)})

    doc = run_fetch(capsys)

    assert doc == {
        "version": 1,
        "active_account": None,
        "pooled_count": 1,
        "pool_next_refill_epoch": None,
        "accounts": [{
            "id": None,
            "label": None,
            "email": "live@example.test",
            "is_active": True,
            "auth_state": "ok",
            "auth_error": None,
            "five_hour_pct": 40,
            "five_hour_reset": "2026-11-20T13:08:12Z",
            "week_pct": 55,
            "week_reset": "2026-11-27T08:08:12Z",
        }],
    }


def test_fallback_failure_prints_null_with_exit_zero(env, capsys):
    # Kein Live-Token, kein Registry-Eintrag: agy meldet Fehler.
    fetch = load_fetch_module()
    capsys.readouterr()

    assert fetch.main() == 0
    assert capsys.readouterr().out.strip() == "null"


def test_registry_failure_prints_null_instead_of_a_partial_document(env, capsys, monkeypatch):
    helpers.two_accounts_saved(env)
    import llm_account as la

    def boom(*_args, **_kwargs):
        raise la.CliError("simulierter Fehler", 3)

    monkeypatch.setattr(la, "fetch_provider_status", boom)
    fetch = load_fetch_module()
    capsys.readouterr()

    assert fetch.main() == 0
    assert capsys.readouterr().out.strip() == "null"


def test_embedded_python_c_run_emits_the_contract_without_file_context(env):
    """Rust bettet den Quelltext per include_str! ein und führt ihn mit `python3 -c` aus (kein __file__)."""
    import subprocess

    helpers.two_accounts_saved(env)
    helpers.set_usage(env, {"a@example.test": helpers.tsv(0, 0, 0, 0), "b@example.test": helpers.tsv(98, 99, 100, 100)})

    proc = subprocess.run(
        [sys.executable, "-c", FETCH_PATH.read_text(encoding="utf-8")],
        # Eingebettet ohne __file__: der Checkout-Ort muss ausdrücklich übergeben werden, unabhängig vom Clone-Pfad.
        env=dict(os.environ, HERDR_NIGHT_WATCH_TOOLS=str(FETCH_PATH.parent)),
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    doc = json.loads(proc.stdout)
    assert doc["active_account"] == "acc2"
    assert doc["pooled_count"] == 2
    assert [a["id"] for a in doc["accounts"]] == ["acc1", "acc2"]


def test_relogin_account_carries_a_user_facing_auth_error(env, capsys):
    helpers.two_accounts_saved(env)  # aktiv acc2
    (env.state / "profiles/agy/acc1/.gemini/antigravity-cli/antigravity-oauth-token").unlink()
    helpers.set_usage(env, {"b@example.test": helpers.tsv(98, 99, 100, 100)})

    doc = run_fetch(capsys)

    by_id = {a["id"]: a for a in doc["accounts"]}
    assert by_id["acc1"]["auth_state"] == "relogin_required"
    assert by_id["acc1"]["auth_error"] == "Login erneuern"
    assert by_id["acc2"]["auth_state"] == "ok"
    assert by_id["acc2"]["auth_error"] is None


def test_without_the_llm_module_no_agy_process_is_started(env, capsys, monkeypatch):
    fetch = load_fetch_module()

    def no_module():
        raise ImportError("simuliert")

    started = []
    monkeypatch.setattr(fetch, "_import_llm_account", no_module)
    monkeypatch.setattr(fetch.subprocess, "run", lambda *a, **k: started.append(a) or None)
    capsys.readouterr()

    assert fetch.main() == 0
    assert capsys.readouterr().out.strip() == "null"
    assert started == []
