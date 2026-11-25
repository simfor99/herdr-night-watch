"""Tests für tools/llm_account.py. Vollständig hermetisch: eigener State, eigenes Live-Home, fake agy."""

import datetime as dt
import json
import os
import subprocess
import sys
import time

from pathlib import Path
from types import SimpleNamespace

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import llm_account as la  # noqa: E402

# Fake Windows profile for the Chrome debug-profile test (passed through LLM_ACCOUNT_CHROME_PROFILE).
CHROME_PROFILE_FIXTURE = r"C:\Users\Alice\AppData\Local\Google\Chrome-Debug-Profile"

FAKE_AGY_TEMPLATE = r'''#!__PY__
import json, os, sys, time

log = os.environ.get("FAKE_AGY_LOG")
if log:
    with open(log, "a", encoding="utf-8") as fh:
        fh.write(json.dumps({"argv": sys.argv[1:], "home": os.environ.get("HOME")}) + "\n")

home = os.environ["HOME"]
token = os.path.join(home, ".gemini/antigravity-cli/antigravity-oauth-token")
scope = os.path.join(home, ".config/antigravity/credential-user-scope")

login_email = os.environ.get("FAKE_AGY_LOGIN_EMAIL")
if login_email:
    os.makedirs(os.path.dirname(token), exist_ok=True)
    os.makedirs(os.path.dirname(scope), exist_ok=True)
    with open(token, "w", encoding="utf-8") as fh:
        json.dump({"auth_method": "oauth", "id_token_email": login_email,
                   "token": {"access_token": "SECRET-LOGIN", "refresh_token": "SECRET-LOGIN"}}, fh)
    with open(scope, "w", encoding="utf-8") as fh:
        fh.write("scope-" + login_email)
    print("login done")
    sys.exit(0)

if os.environ.get("FAKE_AGY_MODE") == "hang":
    time.sleep(60)
if not os.path.exists(token):
    print("Please login first", file=sys.stderr)
    sys.exit(1)
with open(token, encoding="utf-8") as fh:
    email = json.load(fh)["id_token_email"]
with open(os.environ["FAKE_AGY_USAGE_FILE"], encoding="utf-8") as fh:
    table = json.load(fh)
lines = table.get(email)
if lines is None:
    print("token expired, please login", file=sys.stderr)
    sys.exit(1)
print("\n".join(lines))
'''

DECOY_AGY = "#!/bin/sh\necho 'decoy agy on PATH must not be used' >&2\nexit 99\n"

EPOCH_NOW = int(dt.datetime(2026, 11, 20, 9, 0, 0, tzinfo=dt.timezone.utc).timestamp())


def write_account(root: Path, email: str, marker: str = "") -> None:
    token = root / ".gemini/antigravity-cli/antigravity-oauth-token"
    scope = root / ".config/antigravity/credential-user-scope"
    token.parent.mkdir(parents=True, exist_ok=True)
    scope.parent.mkdir(parents=True, exist_ok=True)
    token.write_text(
        json.dumps({"auth_method": "oauth", "id_token_email": email,
                    "token": {"access_token": f"SECRET-AT-{email}{marker}", "refresh_token": f"SECRET-RT-{email}"}}),
        encoding="utf-8",
    )
    scope.write_text(f"scope-{email}", encoding="utf-8")


def tsv(g5, gw, c5, cw, reset5="2026-11-20T13:08:12Z", resetw="2026-11-27T08:08:12Z"):
    return [
        f"Gemini Models\tFive Hour Limit Remaining\t{g5}%\t{reset5}",
        f"Gemini Models\tWeekly Limit Remaining\t{gw}%\t{resetw}",
        f"Claude and GPT models\tFive Hour Limit Remaining\t{c5}%\t{reset5}",
        f"Claude and GPT models\tWeekly Limit Remaining\t{cw}%\t{resetw}",
    ]


@pytest.fixture
def env(tmp_path, monkeypatch):
    fake = tmp_path / "bin" / "agy"
    fake.parent.mkdir()
    fake.write_text(FAKE_AGY_TEMPLATE.replace("__PY__", sys.executable), encoding="utf-8")
    fake.chmod(0o755)
    decoy_dir = tmp_path / "decoy"
    decoy_dir.mkdir()
    (decoy_dir / "agy").write_text(DECOY_AGY, encoding="utf-8")
    (decoy_dir / "agy").chmod(0o755)
    usage = tmp_path / "usage.json"
    usage.write_text("{}", encoding="utf-8")
    log = tmp_path / "agy.log"

    monkeypatch.setenv("LLM_ACCOUNTS_DIR", str(tmp_path / "state"))
    monkeypatch.setenv("LLM_ACCOUNTS_HOME", str(tmp_path / "live"))
    monkeypatch.setenv("MOTHR_AGY_SWITCH_DELTAS_FILE", str(tmp_path / "mothr" / "agy-account-switch-deltas.jsonl"))
    monkeypatch.setenv("AGY_REAL_BIN", str(fake))
    monkeypatch.setenv("FAKE_AGY_USAGE_FILE", str(usage))
    monkeypatch.setenv("FAKE_AGY_LOG", str(log))
    monkeypatch.setenv("PATH", f"{decoy_dir}{os.pathsep}{os.environ.get('PATH', '')}")
    monkeypatch.delenv("FAKE_AGY_LOGIN_EMAIL", raising=False)
    monkeypatch.delenv("FAKE_AGY_MODE", raising=False)
    (tmp_path / "live").mkdir()
    return SimpleNamespace(
        state=tmp_path / "state",
        live=tmp_path / "live",
        usage=usage,
        log=log,
        adapter=la.ADAPTERS["agy"],
    )


def set_usage(env, table):
    env.usage.write_text(json.dumps(table), encoding="utf-8")


def live_token(env) -> Path:
    return env.live / ".gemini/antigravity-cli/antigravity-oauth-token"


def profile_token(env, slot) -> Path:
    return env.state / "profiles/agy" / slot / ".gemini/antigravity-cli/antigravity-oauth-token"


def registry(env) -> dict:
    return json.loads((env.state / "accounts.json").read_text(encoding="utf-8"))["providers"]["agy"]


def events(env) -> list[dict]:
    path = env.state / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def two_accounts_saved(env):
    """acc1 = personal (A), acc2 = work (B); B ist aktiv."""
    write_account(env.live, "a@example.test")
    la.main(["save", "agy", "acc1", "--label", "personal"])
    write_account(env.live, "b@example.test")
    la.main(["save", "agy", "acc2", "--label", "work"])


def test_save_registers_active_slot_and_stores_secrets_with_personale_modes(env, capsys):
    write_account(env.live, "a@example.test")

    assert la.main(["save", "agy", "acc1", "--label", "personal"]) == 0

    block = registry(env)
    assert block["active_account"] == "acc1"
    assert block["slots"] == [{"id": "acc1", "label": "personal", "email": "a@example.test", "state": "active"}]
    assert profile_token(env, "acc1").read_text() == live_token(env).read_text()
    assert oct(profile_token(env, "acc1").stat().st_mode & 0o777) == "0o600"
    assert oct(profile_token(env, "acc1").parent.stat().st_mode & 0o777) == "0o700"
    assert oct(env.state.stat().st_mode & 0o777) == "0o700"
    assert "SECRET" not in capsys.readouterr().out


def test_save_refuses_a_second_slot_for_an_email_already_registered(env):
    write_account(env.live, "a@example.test")
    la.main(["save", "agy", "acc1"])

    assert la.main(["save", "agy", "acc2"]) == 4


def test_slot_ids_that_escape_the_state_dir_are_rejected(env):
    write_account(env.live, "a@example.test")

    assert la.main(["save", "agy", "../evil"]) == 2
    assert not (env.state / "profiles").exists() or not any((env.state / "profiles").rglob("*evil*"))


def test_use_swaps_live_files_updates_registry_and_drops_only_quota_and_resolver_snapshots(env):
    two_accounts_saved(env)
    quota_cache = env.live / ".cache/quota-axi/quotas.json"
    resolver = env.live / ".local/state/herdr/dispatch-resolve-cache"
    quota_cache.parent.mkdir(parents=True)
    quota_cache.write_text("{}")
    resolver.mkdir(parents=True)
    (resolver / "quota-axi-refresh.state.json").write_text("{}")
    (resolver / "dispatch.lock").write_text("")
    (resolver / "route-simple.json").write_text("{}")

    assert la.main(["use", "agy", "acc1"]) == 0

    assert "a@example.test" in live_token(env).read_text()
    assert registry(env)["active_account"] == "acc1"
    assert registry(env)["switch_epoch"] is not None
    assert not quota_cache.exists()
    assert not (resolver / "route-simple.json").exists()
    assert (resolver / "quota-axi-refresh.state.json").exists()
    assert (resolver / "dispatch.lock").exists()
    assert any(e["event"] == "switch" and e["to_slot"] == "acc1" for e in events(env))


def test_use_refuses_retired_and_unknown_slots(env):
    two_accounts_saved(env)
    la.main(["retire", "agy", "acc1"])

    assert la.main(["use", "agy", "acc1"]) == 4
    assert la.main(["use", "agy", "acc9"]) == 4


def test_heal_syncs_a_newer_live_token_back_into_the_active_profile(env):
    write_account(env.live, "a@example.test")
    la.main(["save", "agy", "acc1"])
    write_account(env.live, "a@example.test", marker="-REFRESHED")
    future = time.time() + 120
    os.utime(live_token(env), (future, future))
    live_before = live_token(env).read_text()

    assert la.main(["heal", "agy"]) == 0

    assert profile_token(env, "acc1").read_text() == live_before
    assert live_token(env).read_text() == live_before


def test_heal_rescues_clobbered_token_into_its_origin_slot_and_restores_the_active_account(env, capsys):
    two_accounts_saved(env)  # aktiv: acc2 (B)
    write_account(env.live, "a@example.test", marker="-CLOBBER")  # Alt-Prozess schreibt A-Token in Live-Datei

    assert la.main(["heal", "agy"]) == 0

    assert "a@example.test" in profile_token(env, "acc1").read_text()
    assert "-CLOBBER" in profile_token(env, "acc1").read_text()
    assert "b@example.test" in live_token(env).read_text()
    recovered = [e for e in events(env) if e["event"] == "clobber_recovered"]
    assert recovered and recovered[0]["rescued_to"] == "acc1" and recovered[0]["active"] == "acc2"
    assert "SECRET" not in (env.state / "events.jsonl").read_text()


def test_heal_quarantines_an_unknown_clobbering_account_before_restoring_active(env):
    two_accounts_saved(env)
    write_account(env.live, "fremd@example.test")

    assert la.main(["heal", "agy"]) == 0

    quarantined = list((env.state / "quarantine").glob("*/" + ".gemini/antigravity-cli/antigravity-oauth-token"))
    assert len(quarantined) == 1 and "fremd@example.test" in quarantined[0].read_text()
    assert "b@example.test" in live_token(env).read_text()
    assert [e for e in events(env) if e["event"] == "clobber_recovered"][0]["quarantined"] is True


def test_retire_refuses_the_last_active_slot_without_force_and_shrinks_the_pool_otherwise(env):
    two_accounts_saved(env)  # aktiv acc2

    assert la.main(["retire", "agy", "acc1"]) == 0
    assert [s["state"] for s in registry(env)["slots"]] == ["retired", "active"]
    assert la.main(["retire", "agy", "acc2"]) == 4  # letzter aktiver Slot
    assert la.main(["retire", "agy", "acc2", "--force"]) == 0


def test_login_runs_isolated_agy_seeds_onboarding_and_leaves_the_live_account_alone(env):
    write_account(env.live, "a@example.test")
    la.main(["save", "agy", "acc1", "--label", "personal"])
    live_before = live_token(env).read_text()
    os.environ["FAKE_AGY_LOGIN_EMAIL"] = "b@example.test"
    try:
        assert la.main(["login", "agy", "acc2", "--label", "work"]) == 0
    finally:
        del os.environ["FAKE_AGY_LOGIN_EMAIL"]

    assert live_token(env).read_text() == live_before
    assert registry(env)["active_account"] == "acc1"
    slot2 = env.state / "profiles/agy/acc2"
    assert "b@example.test" in (slot2 / ".gemini/antigravity-cli/antigravity-oauth-token").read_text()
    onboarding = json.loads((slot2 / ".gemini/antigravity-cli/cache/onboarding.json").read_text())
    assert onboarding["onboardingComplete"] is True
    assert not list((env.state / "profiles/agy").glob(".login-*"))
    run = [json.loads(line) for line in env.log.read_text().splitlines()][-1]
    assert run["argv"] == ["-p", "/usage"]
    assert run["home"] != str(env.live)


def test_status_parses_both_lanes_and_derives_pool_numbers_from_the_documented_formulas(env):
    two_accounts_saved(env)  # aktiv acc2
    set_usage(env, {
        "a@example.test": tsv(0, 0, 0, 0, reset5="2026-11-23T23:29:00Z", resetw="2026-11-23T23:29:00Z"),
        "b@example.test": tsv(98, 99, 100, 100),
    })

    snap = la.fetch_provider_status(env.adapter, write_quotas=True, now=EPOCH_NOW)

    prov = snap["providers"]["agy"]
    assert prov["active_account"] == "acc2"
    gemini = prov["effective"]["lanes"]["gemini"]
    claude = prov["effective"]["lanes"]["claude_gpt"]
    assert gemini["pooled_count"] == 2
    assert gemini["spendable_count"] == 1
    assert gemini["effective_weekly_pct"] == 99
    assert gemini["next_refill_at"] == "2026-11-23T23:29:00Z"
    assert gemini["steady_daily_pct"] == 24.3
    assert gemini["bridge_daily_pct"] == 23.3
    assert gemini["sustainable_daily_pct"] == 23.3
    assert gemini["recommended_slot_fefo"] == "acc2"
    assert claude["bridge_daily_pct"] == 23.6
    assert claude["recommended_slot_fefo"] == "acc2"
    assert (env.state / "quotas.json").exists()
    assert "SECRET" not in (env.state / "quotas.json").read_text()


def test_status_calls_the_real_agy_binary_and_never_the_path_wrapper(env):
    two_accounts_saved(env)
    set_usage(env, {"b@example.test": tsv(98, 99, 100, 100), "a@example.test": tsv(0, 0, 0, 0)})

    snap = la.fetch_provider_status(env.adapter, now=EPOCH_NOW)

    assert snap["providers"]["agy"]["accounts"][1]["lanes"]["gemini"]["weekly_pct"] == 99
    homes = [json.loads(line)["home"] for line in env.log.read_text().splitlines()]
    assert set(homes) == {str(env.state / "profiles/agy/acc1"), str(env.state / "profiles/agy/acc2")}


def test_status_marks_a_slot_without_token_as_relogin_required_and_never_starts_agy_for_it(env):
    two_accounts_saved(env)
    profile_token(env, "acc1").unlink()
    set_usage(env, {"b@example.test": tsv(98, 99, 100, 100)})

    snap = la.fetch_provider_status(env.adapter, now=EPOCH_NOW)

    states = {a["id"]: a["auth_state"] for a in snap["providers"]["agy"]["accounts"]}
    assert states == {"acc1": "relogin_required", "acc2": "ok"}
    homes = [json.loads(line)["home"] for line in env.log.read_text().splitlines()]
    assert str(env.state / "profiles/agy/acc1") not in homes


def test_status_marks_a_rejected_token_as_relogin_required(env):
    two_accounts_saved(env)
    set_usage(env, {"b@example.test": tsv(98, 99, 100, 100)})  # acc1 fehlt in der Tabelle -> agy meldet Auth-Fehler

    snap = la.fetch_provider_status(env.adapter, now=EPOCH_NOW)

    states = {a["id"]: a["auth_state"] for a in snap["providers"]["agy"]["accounts"]}
    assert states["acc1"] == "relogin_required"
    assert states["acc2"] == "ok"


def test_status_timeout_is_unavailable_and_excluded_from_the_pool(env, monkeypatch):
    two_accounts_saved(env)
    monkeypatch.setenv("FAKE_AGY_MODE", "hang")
    monkeypatch.setattr(la, "USAGE_TIMEOUT_S", 0.5)
    set_usage(env, {"a@example.test": tsv(0, 0, 0, 0), "b@example.test": tsv(98, 99, 100, 100)})

    snap = la.fetch_provider_status(env.adapter, now=EPOCH_NOW)

    gemini = snap["providers"]["agy"]["effective"]["lanes"]["gemini"]
    assert gemini["pooled_count"] == 0
    assert gemini["recommended_slot_fefo"] is None
    assert {a["auth_state"] for a in snap["providers"]["agy"]["accounts"]} == {"unavailable"}


def test_stale_panes_reads_starttime_past_parenthesised_comm_and_filters_by_switch_epoch(tmp_path, monkeypatch):
    proc = tmp_path / "proc"
    proc.mkdir()
    btime = 1_795_000_000
    (proc / "stat").write_text(f"cpu 1 2 3\nbtime {btime}\n", encoding="utf-8")
    ticks = os.sysconf("SC_CLK_TCK")

    def make_proc(pid, comm, argv, starttime, environ=b""):
        entry = proc / str(pid)
        entry.mkdir()
        after = ["S"] + ["0"] * 18 + [str(starttime)] + ["0"] * 5
        (entry / "stat").write_text(f"{pid} ({comm}) " + " ".join(after) + "\n", encoding="utf-8")
        (entry / "cmdline").write_bytes(b"\0".join(a.encode() for a in argv) + b"\0")
        (entry / "environ").write_bytes(environ)

    make_proc(4242, "agy (x) y", ["/usr/local/bin/agy", "--conversation", "conv-1"], 500 * ticks,
              b"HERDR_PANE_ID=w1:p2\0HERDR_TAB_ID=w1:t1\0OTHER=1\0")
    make_proc(4343, "agy", ["/usr/local/bin/agy"], 2000 * ticks)  # nach dem Wechsel gestartet
    make_proc(99, "bash", ["bash"], 100 * ticks)

    found = la.find_stale_panes(proc, switch_epoch=btime + 1000)

    assert len(found) == 1
    assert found[0]["pid"] == 4242
    assert found[0]["start_epoch"] == btime + 500
    assert found[0]["conversation_id"] == "conv-1"
    assert found[0]["cmdline_kind"] == "conversation"
    assert found[0]["pane_id"] == "w1:p2"
    assert found[0]["tab_id"] == "w1:t1"


def test_install_watcher_dry_run_writes_nothing_and_units_use_home_specifiers(env, tmp_path):
    unit_dir = tmp_path / "units"
    assert "PathChanged=%h/.gemini/antigravity-cli/antigravity-oauth-token" in la.cmd_install_watcher(
        env.adapter, dry_run=True, unit_dir=str(unit_dir)
    )
    assert not unit_dir.exists()

    la.cmd_install_watcher(env.adapter, dry_run=False, unit_dir=str(unit_dir))

    service = (unit_dir / "llm-account-agy-heal.service").read_text()
    assert "ExecStart=%h/.local/bin/llm-account heal agy" in service
    assert oct((unit_dir / "llm-account-agy-heal.path").stat().st_mode & 0o777) == "0o644"


def _record_systemctl(monkeypatch):
    calls = []
    monkeypatch.setattr(la, "_systemctl_user", lambda args, *, check=True: calls.append((tuple(args), check)))
    return calls


def test_heal_timer_units_are_inotify_free_and_use_the_interval(env, tmp_path):
    units = la.heal_timer_units(env.adapter, 30)

    timer = units["llm-account-agy-heal.timer"]
    assert "OnUnitActiveSec=30s" in timer
    assert "Unit=llm-account-agy-heal.service" in timer
    assert "WantedBy=timers.target" in timer
    assert "PathChanged" not in timer
    assert "ExecStart=%h/.local/bin/llm-account heal agy" in units["llm-account-agy-heal.service"]


def test_install_heal_timer_without_systemctl_writes_units_and_calls_nothing(env, tmp_path, monkeypatch):
    calls = _record_systemctl(monkeypatch)
    unit_dir = tmp_path / "units"

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(unit_dir), activate=False, as_json=True))

    assert calls == []
    assert out["activated"] is False
    assert out["interval_seconds"] == 30
    assert "OnUnitActiveSec=30s" in (unit_dir / "llm-account-agy-heal.timer").read_text()
    assert oct((unit_dir / "llm-account-agy-heal.timer").stat().st_mode & 0o777) == "0o644"
    assert not (unit_dir / "llm-account-agy-heal.path").exists()


def test_install_heal_timer_activates_daemon_reload_then_enable_timer(env, tmp_path, monkeypatch):
    calls = _record_systemctl(monkeypatch)
    monkeypatch.setattr(la, "DEFAULT_UNIT_DIR", str(tmp_path))  # Standardverzeichnis: Timer per Basename aktivieren

    la.cmd_install_heal_timer(env.adapter, 60, str(tmp_path), activate=True, as_json=False)

    assert calls == [(("daemon-reload",), True), (("enable", "--now", "llm-account-agy-heal.timer"), True)]


def test_install_heal_timer_removes_only_our_legacy_path_unit_and_disables_it_first(env, tmp_path, monkeypatch):
    calls = _record_systemctl(monkeypatch)
    legacy = tmp_path / "llm-account-agy-heal.path"
    # Echter Legacy-Inhalt: derselbe Text, den install-watcher früher erzeugt hat.
    legacy.write_text(la.watcher_units(env.adapter)["llm-account-agy-heal.path"])

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=True, as_json=True))

    assert not legacy.exists()
    assert out["legacy_path_unit_removed"] is True
    assert calls[0] == (("disable", "--now", "llm-account-agy-heal.path"), False)


def test_install_heal_timer_keeps_a_path_file_that_is_not_our_unit(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    foreign = tmp_path / "llm-account-agy-heal.path"
    foreign.write_text("[Unit]\nDescription=fremd\n")

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=False, as_json=True))

    assert foreign.read_text() == "[Unit]\nDescription=fremd\n"
    assert out["legacy_path_unit_removed"] is False


def test_install_heal_timer_rejects_intervals_outside_the_range_and_writes_nothing(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    unit_dir = tmp_path / "units"

    for bad in (0, 4, 3601):
        with pytest.raises(la.CliError) as exc:
            la.cmd_install_heal_timer(env.adapter, bad, str(unit_dir), activate=False, as_json=False)
        assert exc.value.exit_code == 2
    assert not unit_dir.exists()


def test_systemctl_failure_is_reported_with_exit_3(monkeypatch):
    def failing(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 1, stdout="", stderr="Failed to connect to bus")

    monkeypatch.setattr(la.subprocess, "run", failing)

    with pytest.raises(la.CliError) as exc:
        la._systemctl_user(["daemon-reload"])
    assert exc.value.exit_code == 3
    assert "daemon-reload" in str(exc.value)


def test_inotify_limit_is_read_or_reported_as_unknown(tmp_path):
    limit = tmp_path / "max_user_instances"
    limit.write_text("128\n")

    assert la.inotify_instance_limit(limit) == 128
    assert la.inotify_instance_limit(tmp_path / "fehlt") is None


def _legacy_wants_fixture(unit_dir: Path, name: str = "llm-account-agy-heal.path") -> None:
    for wants in ("default.target.wants", "timers.target.wants"):
        (unit_dir / wants).mkdir(parents=True, exist_ok=True)
        (unit_dir / wants / name).symlink_to(f"../{name}")  # Ziel existiert nicht mehr (tot)


def test_install_heal_timer_cleans_legacy_wants_links_and_links_the_timer_without_systemctl(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    _legacy_wants_fixture(tmp_path)

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=False, as_json=True))

    assert not (tmp_path / "default.target.wants" / "llm-account-agy-heal.path").is_symlink()
    assert not (tmp_path / "timers.target.wants" / "llm-account-agy-heal.path").is_symlink()
    link = tmp_path / "timers.target.wants" / "llm-account-agy-heal.timer"
    assert link.is_symlink() and os.readlink(link) == "../llm-account-agy-heal.timer"
    assert link.exists()
    assert sorted(out["removed_legacy_wants"]) == [
        "default.target.wants/llm-account-agy-heal.path",
        "timers.target.wants/llm-account-agy-heal.path",
    ]
    assert out["timer_wants_link"] == "angelegt"


def test_install_heal_timer_survives_failing_systemctl_with_valid_json_and_no_exception(env, tmp_path, monkeypatch):
    _legacy_wants_fixture(tmp_path)

    def dead_bus(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 1, stdout="", stderr="Failed to connect to bus: No such file or directory")

    monkeypatch.setattr(la.subprocess, "run", dead_bus)

    raw = la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=True, as_json=True)
    out = json.loads(raw)

    assert out["activated"] is False
    assert "Failed to connect to bus" in out["systemctl_error"]
    assert (tmp_path / "llm-account-agy-heal.timer").exists()
    assert (tmp_path / "timers.target.wants" / "llm-account-agy-heal.timer").is_symlink()
    assert not (tmp_path / "default.target.wants" / "llm-account-agy-heal.path").is_symlink()


def test_orphaned_wants_link_is_removed_even_after_the_path_file_is_already_gone(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    _legacy_wants_fixture(tmp_path)
    assert not (tmp_path / "llm-account-agy-heal.path").exists()

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=False, as_json=True))

    assert out["legacy_path_unit_removed"] is False
    assert len(out["removed_legacy_wants"]) == 2
    assert not (tmp_path / "default.target.wants" / "llm-account-agy-heal.path").is_symlink()


def test_foreign_live_wants_link_is_kept(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    wants = tmp_path / "default.target.wants"
    wants.mkdir(parents=True)
    other = tmp_path / "fremd.service"
    other.write_text("[Unit]\n")
    (wants / "llm-account-agy-heal.path").symlink_to(other)

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=False, as_json=True))

    assert (wants / "llm-account-agy-heal.path").is_symlink()
    assert out["removed_legacy_wants"] == []


def test_timer_unit_has_on_boot_and_five_second_accuracy_and_no_path_triggers(env):
    timer = la.heal_timer_units(env.adapter, 45)["llm-account-agy-heal.timer"]

    assert "OnBootSec=10s" in timer
    assert "OnUnitActiveSec=45s" in timer
    assert "AccuracySec=5s" in timer
    assert "Path" not in timer


def test_inotify_warning_below_512_and_none_at_or_above(env, tmp_path, monkeypatch):
    _record_systemctl(monkeypatch)
    for limit, expect_warning in ((128, True), (4096, False)):
        monkeypatch.setattr(la, "inotify_instance_limit", lambda limit=limit: limit)
        out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path / str(limit)), activate=False, as_json=True))
        assert (out["inotify_warning"] is not None) is expect_warning
        if expect_warning:
            assert "max_user_instances=128" in out["inotify_warning"]


def test_foreign_path_unit_and_its_wants_links_are_left_untouched(env, tmp_path, monkeypatch):
    calls = _record_systemctl(monkeypatch)
    foreign = tmp_path / "llm-account-agy-heal.path"
    foreign.write_text(
        "[Unit]\nDescription=fremd\n[Path]\nPathChanged=%h/Documents/inbox\n[Install]\nWantedBy=default.target\n"
    )
    wants = tmp_path / "default.target.wants"
    wants.mkdir()
    (wants / "llm-account-agy-heal.path").symlink_to("../llm-account-agy-heal.path")

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=True, as_json=True))

    assert "PathChanged=%h/Documents/inbox" in foreign.read_text()
    assert (wants / "llm-account-agy-heal.path").is_symlink()
    assert out["legacy_path_unit_removed"] is False
    assert out["removed_legacy_wants"] == []
    assert not any("llm-account-agy-heal.path" in " ".join(args) for args, _ in calls)


def test_custom_unit_dir_links_absolute_service_and_enables_absolute_timer(env, tmp_path, monkeypatch):
    calls = _record_systemctl(monkeypatch)

    out = json.loads(la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=True, as_json=True))

    service = (tmp_path / "llm-account-agy-heal.service").resolve()
    timer = (tmp_path / "llm-account-agy-heal.timer").resolve()
    assert calls == [
        (("daemon-reload",), True),
        (("link", str(service)), True),
        (("enable", "--now", str(timer)), True),
    ]
    assert out["activation_command"] == f"systemctl --user link {service} && systemctl --user enable --now {timer}"


def test_custom_unit_dir_recovery_hint_names_both_absolute_commands(env, tmp_path, monkeypatch):
    def failing(args, *, check=True):
        raise la.CliError("systemctl --user daemon-reload fehlgeschlagen (Exit 1): Failed to connect to bus", 3)

    monkeypatch.setattr(la, "_systemctl_user", failing)
    service = (tmp_path / "llm-account-agy-heal.service").resolve()
    timer = (tmp_path / "llm-account-agy-heal.timer").resolve()

    text = la.cmd_install_heal_timer(env.adapter, 30, str(tmp_path), activate=True, as_json=False)

    assert f"systemctl --user link {service} && systemctl --user enable --now {timer}" in text


def test_pure_token_clobber_keeps_origin_scope_and_restores_active_scope(env):
    """Alt-Prozess überschreibt nur das Token; der Scope gehört weiter zum aktiven Konto."""
    two_accounts_saved(env)  # aktiv acc2 (B); Live-Home = B
    (env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel).write_text("SCOPE-A", encoding="utf-8")
    (env.state / "profiles/agy/acc2" / la.AgyAdapter.scope_rel).write_text("SCOPE-B", encoding="utf-8")
    scope_live = env.live / la.AgyAdapter.scope_rel
    scope_live.write_text("SCOPE-B", encoding="utf-8")
    live_token(env).write_text(
        json.dumps({"auth_method": "oauth", "id_token_email": "a@example.test",
                    "token": {"access_token": "SECRET-AT-A-CLOBBER"}}),
        encoding="utf-8",
    )

    assert la.main(["heal", "agy"]) == 0

    assert "SECRET-AT-A-CLOBBER" in profile_token(env, "acc1").read_text()
    assert (env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel).read_text() == "SCOPE-A"
    assert "b@example.test" in live_token(env).read_text()
    assert scope_live.read_text() == "SCOPE-B"


def test_origin_scope_is_not_filled_from_the_active_accounts_live_scope(env):
    """Der Live-Scope gehört zum aktiven Konto (B). Ein Alt-Prozess-Token von A darf ihn nicht in Slot A kopieren."""
    two_accounts_saved(env)
    origin_scope = env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel
    origin_scope.unlink()
    live_token(env).write_text(
        json.dumps({"auth_method": "oauth", "id_token_email": "a@example.test", "token": {}}), encoding="utf-8"
    )

    assert la.main(["heal", "agy"]) == 0

    assert not origin_scope.exists()  # fehlender Scope bleibt fehlend
    assert "b@example.test" in live_token(env).read_text()  # aktives Konto B unverändert


def test_invalidate_caches_tolerates_files_vanishing_between_check_and_unlink(env, monkeypatch):
    quota = env.live / ".cache/quota-axi/quotas.json"
    quota.parent.mkdir(parents=True)
    quota.write_text("{}", encoding="utf-8")
    resolver = env.live / ".local/state/herdr/dispatch-resolve-cache"
    resolver.mkdir(parents=True)
    (resolver / "route.json").write_text("{}", encoding="utf-8")

    original_unlink = Path.unlink

    def racing_unlink(self, *args, **kwargs):
        original_unlink(self)  # die Datei verschwindet durch einen Mitbewerber ...
        return original_unlink(self)  # ... bevor unser Löschen greift

    monkeypatch.setattr(Path, "unlink", racing_unlink)
    removed = la.invalidate_caches()

    assert removed == []
    assert not quota.exists()


def poisoned_caches(env) -> tuple[Path, Path]:
    """quota-axi hat im Clobber-Fenster das falsche Konto gemessen; der Resolver hat danach gecacht."""
    quota = env.live / ".cache/quota-axi/quotas.json"
    quota.parent.mkdir(parents=True)
    quota.write_text(json.dumps({"providers": [{"provider": "agy", "windows": [{"remaining": 2}]}]}), encoding="utf-8")
    resolver = env.live / ".local/state/herdr/dispatch-resolve-cache"
    resolver.mkdir(parents=True)
    (resolver / "route.json").write_text("{}", encoding="utf-8")
    (resolver / "quota-axi-refresh.state.json").write_text("{}", encoding="utf-8")
    return quota, resolver


def clobber_live_token(env, email: str) -> None:
    """Ein Alt-Prozess hat das Token eines anderen Kontos in die Live-Datei geschrieben."""
    live_token(env).write_text(
        json.dumps({"auth_method": "oauth", "id_token_email": email, "token": {}}), encoding="utf-8"
    )


@pytest.mark.parametrize("clobber_email", ["a@example.test", "unknown@example.test"])
def test_heal_after_clobber_recovery_drops_poisoned_caches(env, clobber_email):
    """Die im Clobber-Fenster gemessene 2 %-Zeile darf nach der Wiederherstellung nicht im Cache bleiben."""
    two_accounts_saved(env)  # aktiv acc2 (B)
    quota, resolver = poisoned_caches(env)
    clobber_live_token(env, clobber_email)

    assert la.main(["heal", "agy"]) == 0

    assert not quota.exists()
    assert not (resolver / "route.json").exists()
    assert (resolver / "quota-axi-refresh.state.json").exists()  # nur Caches, keine Zustandsdateien
    assert "b@example.test" in live_token(env).read_text()


def test_clobber_recovered_event_records_removed_caches(env):
    two_accounts_saved(env)  # aktiv acc2 (B)
    poisoned_caches(env)
    clobber_live_token(env, "a@example.test")

    assert la.main(["heal", "agy"]) == 0

    record = events(env)[-1]
    assert record["event"] == "clobber_recovered"
    assert record["caches_removed"] >= 1


def test_heal_after_clobber_recovery_unlinks_only_the_symlink_not_its_target(env):
    two_accounts_saved(env)  # aktiv acc2 (B)
    quota, _ = poisoned_caches(env)
    decoy_dir = env.state / "decoy"
    decoy_dir.mkdir(parents=True)
    decoy = decoy_dir / "quotas.json"
    decoy.write_text("{}", encoding="utf-8")
    quota.unlink()
    quota.symlink_to(decoy)
    clobber_live_token(env, "a@example.test")

    assert la.main(["heal", "agy"]) == 0

    assert not quota.is_symlink() and not quota.exists()
    assert decoy.read_text() == "{}"


@pytest.mark.parametrize("outcome", ["current", "synced"])
def test_heal_without_clobber_keeps_quota_cache(env, outcome):
    """Ohne Clobber wurde nichts am Konto geändert; eine gültige Messung bleibt verwendbar."""
    two_accounts_saved(env)  # aktiv acc2 (B)
    quota, resolver = poisoned_caches(env)
    if outcome == "synced":
        token = live_token(env)
        newer = token.stat().st_mtime_ns + 1_000_000_000
        os.utime(token, ns=(newer, newer))

    with la.switch_lock():
        assert la.heal_locked(env.adapter) == outcome

    assert quota.exists()
    assert (resolver / "route.json").exists()
    assert not any(e.get("event") == "clobber_recovered" for e in events(env))


def test_use_on_the_already_active_slot_keeps_switch_epoch(env):
    two_accounts_saved(env)  # aktiv acc2, Live-Dateien = B
    reg = json.loads((env.state / "accounts.json").read_text(encoding="utf-8"))
    reg["providers"]["agy"]["switch_epoch"] = 1000
    (env.state / "accounts.json").write_text(json.dumps(reg), encoding="utf-8")

    assert la.main(["use", "agy", "acc2"]) == 0
    assert registry(env)["switch_epoch"] == 1000  # unveränderte Panes bleiben gültig

    assert la.main(["use", "agy", "acc1"]) == 0  # echter Wechsel setzt die Epoche neu
    assert registry(env)["switch_epoch"] > 1000


def test_login_puts_the_opener_shim_first_on_path_and_sets_browser_only_for_login(env, monkeypatch):
    seen = {}

    def spy(argv, env=None, check=False, **_kwargs):
        seen["env"] = dict(env)
        shim = Path(env["BROWSER"])
        seen["mode"] = oct(shim.stat().st_mode & 0o777)
        seen["text"] = shim.read_text(encoding="utf-8")
        write_account(Path(env["HOME"]), "c@example.test")
        return subprocess.CompletedProcess(argv, 0)

    monkeypatch.setattr(la.subprocess, "run", spy)

    assert la.main(["login", "agy", "acc1"]) == 0

    assert seen["env"]["PATH"].split(os.pathsep)[0] == str(Path(seen["env"]["BROWSER"]).parent)
    assert seen["mode"] == "0o700"
    assert "json/new" in seen["text"] and "http(s)-URLs" in seen["text"]
    assert not Path(seen["env"]["BROWSER"]).exists()  # Staging inkl. Shim nach dem Login entfernt
    assert not live_token(env).exists()  # das Live-Home blieb unberührt


def _shim_file(tmp_path: Path) -> Path:
    shim = tmp_path / "xdg-open"
    shim.write_text(la.opener_shim_source(), encoding="utf-8")
    shim.chmod(0o700)
    return shim


def _free_port() -> int:
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def test_opener_shim_opens_the_url_via_cdp_and_rejects_non_http_urls(tmp_path, monkeypatch):
    import http.server
    import subprocess as sp
    import threading
    import urllib.parse

    seen: list[tuple[str, str]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_PUT(self):  # noqa: N802
            seen.append(("PUT", self.path))
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"{}")

        def log_message(self, *_args):
            return

    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        shim = _shim_file(tmp_path)
        url = "https://accounts.example.test/oauth?client=x&state=y"
        env = dict(os.environ, LLM_ACCOUNTS_CDP_HOST="127.0.0.1",
                   LLM_ACCOUNTS_CDP_PORT=str(server.server_address[1]), LLM_ACCOUNTS_CLIP="")
        done = sp.run([str(shim), url], env=env, capture_output=True, text=True, timeout=30)

        assert done.returncode == 0
        assert seen == [("PUT", "/json/new?" + urllib.parse.quote(url, safe=""))]
        assert "Axi-Chrome geöffnet" in done.stderr

        rejected = sp.run([str(shim), "file:///etc/passwd"], env=env, capture_output=True, text=True, timeout=30)
        assert rejected.returncode == 2
        assert len(seen) == 1  # keine Anfrage für abgelehnte Schemata
    finally:
        server.shutdown()
        server.server_close()


def test_opener_shim_falls_back_to_the_windows_profile_when_cdp_is_unreachable(tmp_path):
    import subprocess as sp

    fake_bin = tmp_path / "fakebin"
    fake_bin.mkdir()
    log = tmp_path / "ps.log"
    ps = fake_bin / "powershell.exe"
    ps.write_text(
        '#!/bin/sh\nfor a in "$@"; do printf \'%s\\0\' "$a"; done >> "$PS_LOG"\n'
        'printf \'\\n%s\\n\' "$LLM_LOGIN_URL" >> "$PS_LOG"\nexit 0\n',
        encoding="utf-8",
    )
    ps.chmod(0o755)
    shim = _shim_file(tmp_path)
    url = "https://accounts.example.test/login"
    env = dict(os.environ, PATH=f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
               LLM_ACCOUNTS_CDP_HOST="127.0.0.1", LLM_ACCOUNTS_CDP_PORT=str(_free_port()),
               LLM_ACCOUNTS_CLIP="", PS_LOG=str(log),
               LLM_ACCOUNT_CHROME_PROFILE=CHROME_PROFILE_FIXTURE)

    done = sp.run([str(shim), url], env=env, capture_output=True, text=True, timeout=60)

    assert done.returncode == 0
    assert "Windows-Chrome-Debugprofil" in done.stderr
    recorded = log.read_text(encoding="utf-8").split("\n")
    assert "Start-Process" in recorded[0].replace("\0", " ")
    assert f'--user-data-dir="{CHROME_PROFILE_FIXTURE}"' in recorded[0]  # Start-Process braucht die Anführungszeichen
    assert "$env:LLM_LOGIN_URL" in recorded[0]
    assert recorded[1] == url
    assert "google-chrome" not in recorded[0]


def _recording_powershell(fake_bin: Path, log: Path) -> None:
    """Fake powershell.exe: schreibt Argumente (NUL-getrennt), LLM_LOGIN_URL und WSLENV in das Log."""
    ps = fake_bin / "powershell.exe"
    ps.write_text(
        '#!/bin/sh\nfor a in "$@"; do printf \'%s\\0\' "$a"; done >> "$PS_LOG"\n'
        'printf \'\\n%s\\n\' "$LLM_LOGIN_URL" >> "$PS_LOG"\n'
        'printf \'%s\\n\' "$WSLENV" >> "$PS_LOG"\nexit 0\n',
        encoding="utf-8",
    )
    ps.chmod(0o755)


def test_opener_shim_resolves_the_windows_profile_through_cmd_exe_without_an_override(tmp_path):
    """Regression: windows_local_app_data() nutzt re.match; fehlt der Import, bricht der Opener mit NameError ab."""
    import subprocess as sp

    fake_bin = tmp_path / "fakebin"
    fake_bin.mkdir()
    log = tmp_path / "ps.log"
    _recording_powershell(fake_bin, log)
    cmd = tmp_path / "cmd.exe"
    cmd.write_text("#!/bin/sh\nprintf '%s\\n' 'C:\\Users\\Alice\\AppData\\Local'\n", encoding="utf-8")
    cmd.chmod(0o755)
    shim = _shim_file(tmp_path)
    env = dict(os.environ, PATH=f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
               LLM_ACCOUNTS_CMD_EXE=str(cmd), LLM_ACCOUNTS_CDP_HOST="127.0.0.1",
               LLM_ACCOUNTS_CDP_PORT=str(_free_port()), LLM_ACCOUNTS_CLIP="", PS_LOG=str(log))
    env.pop("LLM_ACCOUNT_CHROME_PROFILE", None)

    done = sp.run([str(shim), "https://accounts.example.test/login"], env=env, capture_output=True,
                  text=True, timeout=60)

    assert "NameError" not in done.stderr
    assert done.returncode == 0
    assert "Windows-Chrome-Debugprofil" in done.stderr
    recorded = log.read_text(encoding="utf-8")
    assert r'--user-data-dir="C:\Users\Alice\AppData\Local\Google\Chrome-Debug-Profile"' in recorded


def test_opener_shim_forwards_the_login_url_to_windows_and_keeps_existing_wslenv_entries(tmp_path):
    import subprocess as sp

    fake_bin = tmp_path / "fakebin"
    fake_bin.mkdir()
    log = tmp_path / "ps.log"
    _recording_powershell(fake_bin, log)
    shim = _shim_file(tmp_path)
    env = dict(os.environ, PATH=f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
               LLM_ACCOUNTS_CDP_HOST="127.0.0.1", LLM_ACCOUNTS_CDP_PORT=str(_free_port()),
               LLM_ACCOUNTS_CLIP="", PS_LOG=str(log), WSLENV="FOO/u:BAR/p",
               LLM_ACCOUNT_CHROME_PROFILE=CHROME_PROFILE_FIXTURE)

    done = sp.run([str(shim), "https://accounts.example.test/login"], env=env, capture_output=True,
                  text=True, timeout=60)

    assert done.returncode == 0
    recorded = log.read_text(encoding="utf-8").split("\n")
    assert recorded[2] == "FOO/u:BAR/p:LLM_LOGIN_URL"
    shim_ns: dict = {"__name__": "opener_shim"}
    exec(la.OPENER_SHIM_TEMPLATE.replace("__PYTHON__", sys.executable), shim_ns)  # __main__-Guard verhindert Start
    assert shim_ns["wslenv_with_login_url"](None) == "LLM_LOGIN_URL"
    assert shim_ns["wslenv_with_login_url"]("LLM_LOGIN_URL/u:X") == "LLM_LOGIN_URL/u:X"


def test_login_succeeds_when_agy_writes_only_the_oauth_token(env, monkeypatch):
    """agy legt beim /usage-Lauf im Staging nur das Token an, nicht den Scope."""

    def spy(argv, env=None, check=False, **_kwargs):
        token = Path(env["HOME"]) / ".gemini/antigravity-cli/antigravity-oauth-token"
        token.parent.mkdir(parents=True, exist_ok=True)
        token.write_text(json.dumps({"auth_method": "oauth", "id_token_email": "solo@example.test",
                                     "token": {"refresh_token": "SECRET-RT"}}), encoding="utf-8")
        return subprocess.CompletedProcess(argv, 0)

    monkeypatch.setattr(la.subprocess, "run", spy)

    assert la.main(["login", "agy", "acc1", "--label", "personal"]) == 0

    slot = env.state / "profiles/agy/acc1"
    assert "solo@example.test" in (slot / ".gemini/antigravity-cli/antigravity-oauth-token").read_text()
    assert not (slot / la.AgyAdapter.scope_rel).exists()
    assert (slot / ".gemini/antigravity-cli/cache/onboarding.json").exists()
    assert registry(env)["slots"][0]["email"] == "solo@example.test"


def test_use_onto_a_profile_without_scope_removes_the_previous_accounts_scope(env, monkeypatch):
    # acc1 ohne Scope (Login-only), acc2 mit Scope; aktiv ist acc2 mit Live-Scope "SCOPE-B".
    write_account(env.live, "a@example.test")
    (env.live / la.AgyAdapter.scope_rel).unlink()
    la.main(["save", "agy", "acc1"])
    assert not (env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel).exists()
    write_account(env.live, "b@example.test")
    (env.live / la.AgyAdapter.scope_rel).write_text("SCOPE-B", encoding="utf-8")
    la.main(["save", "agy", "acc2"])

    assert la.main(["use", "agy", "acc1"]) == 0

    assert "a@example.test" in live_token(env).read_text()
    assert not (env.live / la.AgyAdapter.scope_rel).exists()  # fremder Scope entfernt, agy legt ihn neu an
    assert registry(env)["active_account"] == "acc1"


def test_heal_on_the_same_account_restores_a_scope_that_reappeared_in_live_home(env):
    write_account(env.live, "a@example.test")
    la.main(["save", "agy", "acc1"])
    profile_scope = env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel
    assert profile_scope.exists()
    # agy legt den Scope im Live-Home neu an, das Token ist aber nicht neuer.
    (env.live / la.AgyAdapter.scope_rel).write_text("SCOPE-NEW", encoding="utf-8")
    past = time.time() - 600
    os.utime(live_token(env), (past, past))
    os.utime(profile_token(env, "acc1"), (time.time(), time.time()))

    assert la.main(["heal", "agy"]) == 0

    assert profile_scope.read_text(encoding="utf-8") == "SCOPE-NEW"


# ---- CodeRabbit-Befunde (Folgecommit): Cooldown-Lock, Dry-Run, Zeitauflösung, Token-Bytes, Snapshot, Quota-Bindung ----


def test_a_second_bounce_inside_the_cooldown_is_refused_and_keeps_the_first_entry(env):
    la.record_bounce_attempt("w1:p7", None, now=1000.0)
    with pytest.raises(la.CliError):
        la.record_bounce_attempt("w1:p7", None, now=1100.0)
    data = json.loads((env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert data["w1:p7"] == {"ts": 1000.0, "switch_epoch": None}


def test_reading_the_cooldown_does_not_create_the_state_directory(env):
    assert not env.state.exists()
    assert la.anti_loop_cooldown("w1:p1", None) == 0.0
    assert not env.state.exists()


def test_mark_active_keeps_the_subsecond_switch_time(monkeypatch):
    monkeypatch.setattr(la.time, "time", lambda: 1_795_000_000.8)
    block = {"active_account": None, "switch_epoch": None, "slots": []}
    la.mark_active(block, "acc1")
    assert block["switch_epoch"] == 1_795_000_000.8


def test_heal_copies_the_token_bytes_it_identified_not_a_later_write(env, monkeypatch):
    two_accounts_saved(env)  # aktiv B
    la.main(["use", "agy", "acc1"])  # aktiv A, Live-Home = A
    past = time.time() - 3600
    os.utime(profile_token(env, "acc1"), (past, past))  # Live-Token ist neuer, also wäre ein Sync fällig
    original_identity = la.identity_from_bytes
    injected = {"done": False}

    def write_after_the_read(raw):
        identity = original_identity(raw)  # Identität aus den bereits gelesenen Bytes (A)
        if not injected["done"]:
            injected["done"] = True
            write_account(env.live, "b@example.test", marker="-RACE")  # Alt-Prozess schreibt B nach dem Lesen
        return identity

    monkeypatch.setattr(la, "identity_from_bytes", write_after_the_read)
    assert la.main(["heal", "agy"]) == 0
    assert injected["done"], "der Schreibvorgang muss tatsächlich zwischen Lesen und Kopieren stattfinden"
    assert "a@example.test" in profile_token(env, "acc1").read_text(encoding="utf-8")


def test_snapshot_is_not_written_when_retire_overtakes_the_probes(env, monkeypatch):
    two_accounts_saved(env)

    def probes_then_retire(adapter, slots):
        la.cmd_retire(adapter, "acc1", force=False)  # überholt die laufende Auswertung
        return {s["id"]: {"auth_state": la.AUTH_OK, "lanes": {}} for s in slots}

    monkeypatch.setattr(la, "probe_slots", probes_then_retire)
    snapshot = la.fetch_provider_status(la.ADAPTERS["agy"], write_quotas=True)

    assert snapshot.get("registry_changed") is True
    assert not (env.state / "quotas.json").exists()


def test_quota_snapshot_for_another_account_does_not_prove_the_live_account(env):
    two_accounts_saved(env)  # aktiv acc2 (B)
    switch_epoch = registry(env)["switch_epoch"]
    snap = {"version": 1, "updated_epoch": time.time(), "providers": {"agy": {
        "active_account": "acc1", "switch_epoch": switch_epoch,
        "accounts": [{"id": "acc1", "auth_state": "ok"}]}}}
    env.state.mkdir(parents=True, exist_ok=True)
    (env.state / "quotas.json").write_text(json.dumps(snap), encoding="utf-8")

    assert "disk_token_unhealthy:quotas_anderes_konto" in la.disk_token_blockers(la.ADAPTERS["agy"], reason="auth_401")


# ---- CodeRabbit Runde 3: AGY_REAL_BIN, Wechsel-Rollback, Subsekunden beim Wechsel ----


def test_a_bare_agy_real_bin_is_refused_before_any_agy_is_started(env, monkeypatch):
    monkeypatch.setenv("AGY_REAL_BIN", "agy")  # nackter Name würde über $PATH laufen
    assert la.main(["status", "agy", "--json"]) == 2
    assert not env.log.exists()  # kein agy gestartet


def test_a_failed_account_switch_leaves_the_old_live_pair_and_the_registry_untouched(env):
    two_accounts_saved(env)  # aktiv B
    (env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel).unlink(missing_ok=True)
    scope_dir = env.state / "profiles/agy/acc1" / la.AgyAdapter.scope_rel
    scope_dir.mkdir(parents=True)  # Scope des Ziels ist nicht lesbar (Verzeichnis statt Datei)
    before_token = live_token(env).read_text(encoding="utf-8")
    before_active = registry(env)["active_account"]

    with pytest.raises(la.CliError):
        la.cmd_use(la.ADAPTERS["agy"], "acc1")

    assert live_token(env).read_text(encoding="utf-8") == before_token  # Live-Token B unverändert
    assert registry(env)["active_account"] == before_active


def test_a_switch_records_the_subsecond_switch_time(env, monkeypatch):
    two_accounts_saved(env)  # aktiv B
    monkeypatch.setattr(la.time, "time", lambda: 1_795_000_000.75)
    la.main(["use", "agy", "acc1"])
    assert registry(env)["switch_epoch"] == 1_795_000_000.75


# ---- Abschluss-Fixes (kontrolleur): Rollback-Pfade, Auth-401-Regex, Limit-Zeilen ----


def _failing_install_after_writing_token(message):
    """Ersetzt install_profile_to_live: schreibt das neue Token ins Live-Home und scheitert danach."""

    def install(adapter, pdir):
        la.atomic_write(la.live_home() / adapter.token_rel, b'{"id_token_email": "neu@example.test"}')
        raise RuntimeError(message)

    return install


def test_rollback_after_a_failed_install_restores_the_old_token_when_live_home_is_outside_the_state_dir(env, monkeypatch):
    """Regression: owner_only im Rollback scheiterte mit ValueError, weil das Live-Home außerhalb des State-Verzeichnisses liegt."""
    two_accounts_saved(env)  # aktiv B, Live-Home und State-Verzeichnis sind getrennt
    before = live_token(env).read_text(encoding="utf-8")
    monkeypatch.setattr(la, "install_profile_to_live", _failing_install_after_writing_token("Platte voll"))

    with pytest.raises(RuntimeError, match="Platte voll"):
        la.cmd_use(la.ADAPTERS["agy"], "acc1")

    assert live_token(env).read_text(encoding="utf-8") == before


def test_rollback_removes_a_token_that_did_not_exist_before_the_switch(env, monkeypatch):
    two_accounts_saved(env)
    live_token(env).unlink()  # vor dem Wechsel gab es kein Live-Token
    monkeypatch.setattr(la, "install_profile_to_live", _failing_install_after_writing_token("Platte voll"))

    with pytest.raises(RuntimeError):
        la.cmd_use(la.ADAPTERS["agy"], "acc1")

    assert not live_token(env).exists()


def test_a_registry_commit_failure_rolls_the_live_files_back(env, monkeypatch):
    two_accounts_saved(env)  # aktiv B
    before = live_token(env).read_text(encoding="utf-8")

    def failing_save(reg):
        raise OSError("Registry nicht schreibbar")

    monkeypatch.setattr(la, "save_registry", failing_save)
    with pytest.raises(OSError, match="Registry nicht schreibbar"):
        la.cmd_use(la.ADAPTERS["agy"], "acc1")

    assert live_token(env).read_text(encoding="utf-8") == before


def test_unknown_limit_rows_do_not_create_a_healthy_lane(env):
    text = "Claude and GPT models\tDaily Limit Remaining\t90%\t2026-11-27T08:08:12Z"
    assert la.parse_usage_tsv(text) == {}


def test_only_real_limit_rows_are_read_for_a_lane(env):
    text = ("Gemini Models\tFive Hour Limit Remaining\t40%\t2026-11-20T13:08:12Z\n"
            "Gemini Models\tDaily Limit Remaining\t90%\t2026-11-27T08:08:12Z")
    lanes = la.parse_usage_tsv(text)
    assert list(lanes) == ["gemini"]
    assert lanes["gemini"]["five_hour_pct"] == 40 and lanes["gemini"]["weekly_pct"] is None
