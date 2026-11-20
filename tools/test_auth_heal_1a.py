"""AUTH-HEAL-1a: Tests für die 7-Bedingungen-Härtung von `bounce-stale` und die Quellen-Härtung
der `powershell.exe`-Aufrufer. Vollständig mit Fake-Herdr und Fake-/proc; kein realer Eingriff."""

# ruff: noqa: F811  (pytest-Fixtures werden aus test_bounce_stale importiert und als Parameter genutzt)

import ast
import hashlib
import json
import sys
import time
from pathlib import Path

import pytest

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import llm_account as la  # noqa: E402
from test_bounce_stale import (  # noqa: E402,F401  (Fixtures und Helfer)
    BTIME, CONV_OLD, MODEL, SWITCH_EPOCH, TICKS, EMPTY_SCREEN, FakeHerdr, add_agy, chat_args, fake_chat, pane, world,
)
from test_llm_account import env  # noqa: E402,F401

AD = la.ADAPTERS["agy"]
TOKEN_REL = Path(".gemini/antigravity-cli/antigravity-oauth-token")


def add_proc(proc, pid, *, ppid, argv, name=None, state="S", start=BTIME + 600):
    """Fügt einen Prozess mit /proc-Status, cmdline und stat in den Fake-/proc ein."""
    entry = proc / str(pid)
    entry.mkdir()
    comm = name or Path(argv[0]).name
    (entry / "status").write_text(f"Name:\t{comm}\nState:\t{state}\nPPid:\t{ppid}\n", encoding="utf-8")
    (entry / "cmdline").write_bytes(b"\0".join(a.encode() for a in argv) + b"\0")
    after = ["S"] + ["0"] * 18 + [str((start - BTIME) * TICKS)] + ["0"] * 5
    (entry / "stat").write_text(f"{pid} ({comm}) " + " ".join(after) + "\n", encoding="utf-8")


def screen_with(*tail_lines):
    return EMPTY_SCREEN + "\n".join(tail_lines) + "\n"


def plan_for(world, *, herdr=None, reason="switch_epoch", source=None, allow_focused=False, replay=None):
    return la.plan_bounce(AD, "w1:p1", herdr or FakeHerdr([pane()]), proc=world.proc, reason=reason,
                          allow_focused=allow_focused, source=source, replay_msg_ids=replay)


# ---- Bedingung 3: Fokus-Schutz ----

def test_focused_pane_blocks_the_bounce(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = plan_for(world, herdr=FakeHerdr([{**pane(), "focused": True}]))
    assert "pane_focused" in plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_allow_focused_overrides_only_the_focus_guard(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = plan_for(world, herdr=FakeHerdr([{**pane(), "focused": True}]), allow_focused=True)
    assert plan["blockers"] == []
    assert plan["recommended_action"] == "in_place_resume"


def test_missing_focus_field_fails_closed(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    entry = {k: v for k, v in pane().items() if k != "focused"}
    plan = plan_for(world, herdr=FakeHerdr([entry]))
    assert "focus_unbekannt" in plan["blockers"]


# ---- Bedingung 4: Nachfahrenbaum und Statuszeile ----

def test_powershell_below_bash_blocks_and_is_listed(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    add_proc(world.proc, 5001, ppid=4242, argv=["bash", "-c", "powershell.exe -Command x"], name="bash")
    add_proc(world.proc, 5002, ppid=5001, argv=["powershell.exe", "-NoProfile"], name="powershell.exe")
    plan = plan_for(world)
    assert "active_or_hung_descendant" in plan["blockers"]
    pids = {d["pid"] for d in plan["active_descendants"]}
    assert {5001, 5002} <= pids
    assert any("powershell.exe" in d["cmd"] for d in plan["active_descendants"])
    assert all({"pid", "age_s", "cpu_s", "cmd"} <= set(d) for d in plan["active_descendants"])


@pytest.mark.parametrize("argv,name", [
    (["node", "/x/mcp-server.js"], "node"),
    (["/usr/bin/bash", "/home/x/statusline.sh"], "bash"),
    (["language_server_linux"], "language_server_linux"),
])
def test_harness_infrastructure_is_not_a_blocker(world, argv, name):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    add_proc(world.proc, 5001, ppid=4242, argv=argv, name=name)
    plan = plan_for(world)
    assert "active_or_hung_descendant" not in plan["blockers"]


def test_zombie_child_is_not_a_blocker(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    add_proc(world.proc, 5001, ppid=4242, argv=["powershell.exe"], name="powershell.exe", state="Z (zombie)")
    plan = plan_for(world)
    assert "active_or_hung_descendant" not in plan["blockers"]


@pytest.mark.parametrize("tail,blocks", [
    ("esc to interrupt", True),
    ("1 task(s) running", True),
    ("0 task(s)", False),
    ("auto · Gemini 4 Neon · high", False),
])
def test_statusline_guard(world, tail, blocks):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = plan_for(world, herdr=FakeHerdr([pane()], screen=screen_with(tail)))
    assert ("active_statusline" in plan["blockers"]) is blocks


# ---- Bedingung 6: Anti-Loop-Cap ----

def _state(env, data):
    env.state.mkdir(parents=True, exist_ok=True)
    (env.state / "bounce-state.json").write_text(json.dumps(data), encoding="utf-8")


def test_recent_bounce_in_same_epoch_blocks_a_second_one(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _state(env, {"w1:p1": {"ts": time.time() - 100, "switch_epoch": SWITCH_EPOCH}})
    plan = plan_for(world)
    assert "anti_loop_cap_active" in plan["blockers"]
    assert 1500 < plan["cooldown_remaining_s"] <= 1800


def test_bounce_older_than_thirty_minutes_is_allowed(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _state(env, {"w1:p1": {"ts": time.time() - 2000, "switch_epoch": SWITCH_EPOCH}})
    assert "anti_loop_cap_active" not in plan_for(world)["blockers"]


def test_new_switch_epoch_lifts_the_cap(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _state(env, {"w1:p1": {"ts": time.time() - 100, "switch_epoch": SWITCH_EPOCH - 1}})
    assert "anti_loop_cap_active" not in plan_for(world)["blockers"]


def test_execute_refuses_when_the_attempt_was_recorded_meanwhile(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])
    plan = plan_for(world, herdr=herdr)
    assert plan["recommended_action"] == "in_place_resume"
    la.record_bounce_attempt("w1:p1", SWITCH_EPOCH)
    with pytest.raises(la.CliError):
        la.execute_bounce(AD, plan, herdr, proc=world.proc)
    assert herdr.calls == []


def test_recorded_attempt_is_persisted_as_json(env):
    la.record_bounce_attempt("w1:p9", SWITCH_EPOCH, now=1000.0)
    data = json.loads((env.state / "bounce-state.json").read_text(encoding="utf-8"))
    assert data["w1:p9"] == {"ts": 1000.0, "switch_epoch": SWITCH_EPOCH}


def test_unreadable_bounce_state_fails_closed(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    env.state.mkdir(parents=True, exist_ok=True)
    (env.state / "bounce-state.json").write_text("{kaputt", encoding="utf-8")
    with pytest.raises(la.CliError):
        plan_for(world)


# ---- Bedingung 1: Token- und quotas.json-Vortest (rein lesend) ----

def _quota(env, *, updated, auth="ok", active="a"):
    """Snapshot gehört zum aktiven Konto der Registry; sonst gilt er nicht als Nachweis (quotas_anderes_konto)."""
    env.state.mkdir(parents=True, exist_ok=True)
    reg_path = env.state / "accounts.json"
    reg = json.loads(reg_path.read_text(encoding="utf-8"))
    block = reg["providers"].setdefault("agy", {"active_account": None, "switch_epoch": None, "slots": []})
    block["active_account"], block["switch_epoch"] = active, SWITCH_EPOCH
    reg_path.write_text(json.dumps(reg), encoding="utf-8")
    snap = {"version": 1, "updated_epoch": updated, "providers": {"agy": {
        "active_account": active, "switch_epoch": SWITCH_EPOCH,
        "accounts": [{"id": active, "auth_state": auth}]}}}
    (env.state / "quotas.json").write_text(json.dumps(snap), encoding="utf-8")


def test_stale_quota_snapshot_blocks(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _quota(env, updated=time.time() - 600)
    assert "disk_token_unhealthy:quotas_veraltet" in plan_for(world)["blockers"]


def test_relogin_required_blocks(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _quota(env, updated=time.time(), auth="relogin_required")
    assert "disk_token_unhealthy:auth_relogin_required" in plan_for(world)["blockers"]


def test_fresh_ok_snapshot_does_not_block(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    _quota(env, updated=time.time(), auth="ok")
    assert not [b for b in plan_for(world)["blockers"] if b.startswith("disk_token_unhealthy")]


def test_token_check_never_changes_the_token_file(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    token = env.live / TOKEN_REL
    token.parent.mkdir(parents=True, exist_ok=True)
    token.write_text(json.dumps({"id_token_email": "a@example.invalid"}), encoding="utf-8")
    _quota(env, updated=time.time() - 600)
    before = hashlib.sha256(token.read_bytes()).hexdigest()
    plan_for(world)
    assert hashlib.sha256(token.read_bytes()).hexdigest() == before


def test_unreadable_live_token_blocks(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    token = env.live / TOKEN_REL
    token.parent.mkdir(parents=True, exist_ok=True)
    token.write_text("{kaputt", encoding="utf-8")
    assert "disk_token_unhealthy:unreadable" in plan_for(world)["blockers"]


# ---- Bedingung 5: UUID-Kollision ----

def test_second_live_process_with_same_conversation_routes_to_mutter_kind(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    add_agy(world.proc, 4343, "w1:p2", cwd=world.cwd)
    plan = plan_for(world)
    assert "conversation_uuid_collision" in plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


# ---- Bedingung 1 und 2: 401-Ausnahme, Tab-Schutz, Zielbindung ----

ARCH_SCREEN = screen_with("401 UNAUTHENTICATED: token expired")


def test_auth_401_allows_in_place_bounce_on_an_architekt_tab(world, env):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd, start_epoch=SWITCH_EPOCH + 100)  # nicht veraltet
    _healthy_token_and_slot(env)
    _quota(env, updated=time.time(), auth="ok")
    herdr = FakeHerdr([pane()], tab="architekt", screen=ARCH_SCREEN)
    plan = plan_for(world, herdr=herdr, reason="auth_401", source="msg-123")
    assert plan["blockers"] == []
    assert plan["recommended_action"] == "in_place_resume"
    assert plan["reason"] == "auth_401"


# ---- S2-1: auth_401 ist fail-closed bei jedem nicht gesunden Disk-Token- und Quota-Zustand ----

def _healthy_token_and_slot(env, email="a@example.invalid"):
    token = env.live / TOKEN_REL
    token.parent.mkdir(parents=True, exist_ok=True)
    token.write_text(json.dumps({"id_token_email": email}), encoding="utf-8")
    env.state.mkdir(parents=True, exist_ok=True)
    (env.state / "accounts.json").write_text(json.dumps({"version": 1, "providers": {"agy": {
        "active_account": "a", "switch_epoch": SWITCH_EPOCH,
        "slots": [{"id": "a", "email": email, "state": "active"}]}}}), encoding="utf-8")


@pytest.mark.parametrize("setup,expected", [
    ("no_token", "disk_token_unhealthy:no_live_token"),
    ("no_active", "disk_token_unhealthy:no_active"),
    ("no_quota", "disk_token_unhealthy:quotas_fehlt"),
])
def test_auth_401_blocks_unhealthy_disk_state(world, env, setup, expected):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd, start_epoch=SWITCH_EPOCH + 100)
    if setup != "no_token":
        _healthy_token_and_slot(env)
    if setup != "no_quota":
        _quota(env, updated=time.time(), auth="ok")
    if setup == "no_active":  # nach _quota, sonst überschreibt der Helfer den Registry-Zustand
        (env.state / "accounts.json").write_text(json.dumps({"version": 1, "providers": {"agy": {
            "active_account": None, "switch_epoch": SWITCH_EPOCH, "slots": []}}}), encoding="utf-8")
    plan = plan_for(world, herdr=FakeHerdr([pane()], tab="architekt", screen=ARCH_SCREEN),
                    reason="auth_401", source="msg-1")
    assert expected in plan["blockers"], plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_switch_epoch_still_tolerates_a_missing_live_token(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    assert "disk_token_unhealthy:no_live_token" not in plan_for(world)["blockers"]


# ---- S3-1: optionales Ablaufdatum in der Token-Datei ----

@pytest.mark.parametrize("value,expected", [
    (time.time() - 60, "disk_token_unhealthy:token_abgelaufen"),
    (time.time() + 3600, None),
    ("kein-datum", "disk_token_unhealthy:ablauf_unlesbar"),
])
def test_token_expiry_is_checked_when_present(world, env, value, expected):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    token = env.live / TOKEN_REL
    token.parent.mkdir(parents=True, exist_ok=True)
    token.write_text(json.dumps({"id_token_email": "a@example.invalid", "expires_at": value}), encoding="utf-8")
    blockers = plan_for(world)["blockers"]
    if expected is None:
        assert not [b for b in blockers if "ablauf" in b or "abgelaufen" in b]
    else:
        assert expected in blockers


# ---- S3-2: protected_human_tab nur für menschliche Tabs ----

def test_protected_human_tab_flag_is_false_for_architekt_tabs(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    assert plan_for(world, herdr=FakeHerdr([pane()], tab="architekt"))["protected_human_tab"] is False
    assert plan_for(world, herdr=FakeHerdr([pane()], tab="vermarkter"))["protected_human_tab"] is True


def test_architekt_tab_stays_protected_for_switch_epoch(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = plan_for(world, herdr=FakeHerdr([pane()], tab="architekt"))
    assert "protected_architekt_tab" in plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_auth_401_without_evidence_in_the_screen_is_refused(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd, start_epoch=SWITCH_EPOCH + 100)
    plan = plan_for(world, herdr=FakeHerdr([pane()], tab="architekt"), reason="auth_401", source="msg-1")
    assert "auth_401_nicht_belegt" in plan["blockers"]


def test_auth_401_without_source_is_refused(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd, start_epoch=SWITCH_EPOCH + 100)
    plan = plan_for(world, herdr=FakeHerdr([pane()], tab="architekt", screen=ARCH_SCREEN), reason="auth_401")
    assert "zielbindung_quelle_fehlt" in plan["blockers"]


def test_human_tab_stays_blocked_even_with_auth_401(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd, start_epoch=SWITCH_EPOCH + 100)
    plan = plan_for(world, herdr=FakeHerdr([pane()], tab="vermarkter", screen=ARCH_SCREEN),
                    reason="auth_401", source="msg-1")
    assert "protected_human_tab" in plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_unknown_reason_is_rejected(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    with pytest.raises(la.CliError):
        plan_for(world, reason="beliebig")


# ---- Bedingung 6 und 7: Weckruf mit abgebrochener Nachricht und Briefkasten-Zähler ----

def test_wakeup_names_the_replayed_message(world, fake_chat):
    herdr = FakeHerdr([pane()])
    result = la._wakeup(herdr, "w1:p1", 1, 1, 0, replay_msg_ids=["msg-1", "msg-2"])
    assert result["wakeup_sent"] is True
    args = chat_args(fake_chat)
    assert any("Abgebrochene Nachricht: msg-1, msg-2." in a for a in args)


def test_wakeup_without_replay_keeps_the_standard_text(world, fake_chat):
    la._wakeup(FakeHerdr([pane()]), "w1:p1", 1, 1, 0)
    assert not any("Abgebrochene Nachricht" in a for a in chat_args(fake_chat))


def test_replay_id_with_shell_characters_is_rejected(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    with pytest.raises(la.CliError):
        plan_for(world, replay=["msg; rm -rf /"])


def test_unread_count_counts_top_level_messages_only(tmp_path, monkeypatch):
    box = tmp_path / "briefkasten" / "w1_p1"
    (box / "gelesen").mkdir(parents=True)
    (box / "a.md").write_text("x", encoding="utf-8")
    (box / "b.md").write_text("x", encoding="utf-8")
    (box / "gelesen" / "c.md").write_text("x", encoding="utf-8")
    monkeypatch.setenv("HERDR_BRIEFKASTEN_DIR", str(tmp_path / "briefkasten"))
    assert la.briefkasten_unread("w1:p1") == 2


# ---- Teil B: powershell.exe-Aufrufer mit stdin=DEVNULL und timeout ----

PS_SITES = {
    "dump_live_windows.py": "processes",
    "verify_tray_quit.py": "main",
    "deploy_tray.py": "ps",
    "llm_account.py": "run_powershell",
}


def _is_subprocess_call(node):
    return (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr in {"run", "check_output", "Popen", "call"}
            and isinstance(node.func.value, ast.Name) and node.func.value.id == "subprocess")


def _source_under_test(name):
    """llm_account.py erzeugt sein Opener-Skript aus OPENER_SHIM_TEMPLATE; dort steht der echte PowerShell-Aufruf."""
    src = (TOOLS / name).read_text(encoding="utf-8")
    if name != "llm_account.py":
        return src
    tree = ast.parse(src)
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", "") == "OPENER_SHIM_TEMPLATE" for t in node.targets):
            return node.value.value
    raise AssertionError("OPENER_SHIM_TEMPLATE nicht gefunden")


def _powershell_calls(name):
    src = _source_under_test(name)
    tree = ast.parse(src)
    sites = []
    for fn in [n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)]:
        for node in ast.walk(fn):
            if _is_subprocess_call(node):
                segment = ast.get_source_segment(src, node) or ""
                if "powershell" in segment.lower() or fn.name == "run_powershell":
                    sites.append((fn.name, node))
    return sites


def _module_constants(src):
    """Modulebene: Name = Konstante. Eine Konstante mit Wert None bleibt sichtbar (und wird abgelehnt)."""
    constants = {}
    for node in ast.parse(src).body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name) \
                and isinstance(node.value, ast.Constant):
            constants[node.targets[0].id] = node.value.value
    return constants


def _unsafe_powershell_reasons(node, constants=None):
    """Gründe, warum ein powershell-Aufruf nicht sicher gebunden ist. Leer heißt: stdin=DEVNULL und endliche Frist.

    Benannte Werte werden über die Modulkonstanten aufgelöst; ist ein Wert nicht auflösbar, gilt er als unsicher.
    """
    constants = constants or {}
    reasons = []
    values = {k.arg: k.value for k in node.keywords}
    stdin = values.get("stdin")
    if not (isinstance(stdin, ast.Attribute) and stdin.attr == "DEVNULL"):
        reasons.append("stdin ist nicht subprocess.DEVNULL")
    timeout = values.get("timeout")
    if timeout is None:
        reasons.append("timeout fehlt")
        return reasons
    if isinstance(timeout, ast.Name):
        if timeout.id not in constants:
            reasons.append(f"timeout {timeout.id} ist nicht auflösbar")
            return reasons
        resolved = constants[timeout.id]
    elif isinstance(timeout, ast.Constant):
        resolved = timeout.value
    else:
        reasons.append("timeout ist kein Literal oder keine Modulkonstante")
        return reasons
    if not (isinstance(resolved, (int, float)) and not isinstance(resolved, bool) and 0 < resolved < float("inf")):
        reasons.append(f"timeout ist keine positive endliche Frist: {resolved!r}")
    return reasons


@pytest.mark.parametrize("name", sorted(PS_SITES))
def test_every_powershell_call_sets_stdin_and_timeout(name):
    src = _source_under_test(name)
    constants = _module_constants(src)
    sites = _powershell_calls(name)
    assert sites, f"{name}: kein powershell-Aufruf gefunden (Prüfung wäre leer)"
    for fn_name, node in sites:
        reasons = _unsafe_powershell_reasons(node, constants)
        assert reasons == [], f"{name}:{fn_name}: {reasons}"


def test_named_none_timeout_is_rejected_like_the_literal_none():
    """Gegenprobe: eine benannte Konstante mit Wert None darf nicht als gebundene Frist gelten."""
    src = "TIMEOUT = None\nsubprocess.run(['powershell'], stdin=subprocess.DEVNULL, timeout=TIMEOUT)\n"
    node = ast.parse(src).body[1].value
    assert _unsafe_powershell_reasons(node, _module_constants(src)) == [
        "timeout ist keine positive endliche Frist: None"]
    unknown = ast.parse("subprocess.run(['powershell'], stdin=subprocess.DEVNULL, timeout=MISSING)").body[0].value
    assert _unsafe_powershell_reasons(unknown, {}) == ["timeout MISSING ist nicht auflösbar"]


def test_the_powershell_check_rejects_none_values_and_a_missing_timeout():
    """Gegenprobe: stdin=None und timeout=None dürfen nicht als gebunden gelten."""
    unsafe = ast.parse("subprocess.run(['powershell'], stdin=None, timeout=None)").body[0].value
    assert _unsafe_powershell_reasons(unsafe) == [
        "stdin ist nicht subprocess.DEVNULL", "timeout ist keine positive endliche Frist: None"]
    no_timeout = ast.parse("subprocess.run(['powershell'], stdin=subprocess.DEVNULL)").body[0].value
    assert _unsafe_powershell_reasons(no_timeout) == ["timeout fehlt"]
    zero = ast.parse("subprocess.run(['powershell'], stdin=subprocess.DEVNULL, timeout=0)").body[0].value
    assert len(_unsafe_powershell_reasons(zero)) == 1


def test_a_stop_timeout_is_reported_as_a_structured_result_without_a_second_stop(monkeypatch, capsys):
    """Regression: subprocess.run(..., timeout=15) warf TimeoutExpired, das Skript endete ohne JSON-Ergebnis."""
    import subprocess as sp

    import verify_tray_quit as vtq

    calls = []
    payload = {"windows": [{"title": "Herdr-Nachtwächter - Live-Status"}],
               "processes": [{"ProcessId": 111, "CommandLine": "herdr-nachtwaechter.exe"}]}

    def fake_run(argv, **kwargs):
        calls.append(argv)
        if argv[0] == "python.exe":
            return sp.CompletedProcess(argv, 0, "", "")
        raise sp.TimeoutExpired(argv, kwargs.get("timeout", 15))  # powershell.exe Stop-Process hängt

    monkeypatch.setattr(vtq.subprocess, "run", fake_run)
    monkeypatch.setattr(vtq, "dump", lambda: payload)
    assert vtq.main() == 3
    out = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert out["reason"] == "stop_timeout" and out["stop_pids"] == [111]
    assert out["remaining_tray_pids"] == [111]
    assert sum(1 for argv in calls if argv[0] == "powershell.exe") == 1  # kein blindes Wiederholen


def test_a_dump_failure_before_the_stop_is_a_structured_result_and_no_stop_runs(monkeypatch, capsys):
    import subprocess as sp

    import verify_tray_quit as vtq

    stops = []

    def fake_run(argv, **kwargs):
        if argv[0] == "powershell.exe":
            stops.append(argv)
        return sp.CompletedProcess(argv, 0, "", "")

    def dump_times_out():
        raise sp.TimeoutExpired(["python.exe"], 15)

    monkeypatch.setattr(vtq.subprocess, "run", fake_run)
    monkeypatch.setattr(vtq, "dump", dump_times_out)
    assert vtq.main() == 2
    out = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert out["reason"] == "dump_failed" and out["stage"] == "before_stop"
    assert stops == []


def test_a_dump_failure_after_the_stop_reports_unknown_state_without_repeating_the_stop(monkeypatch, capsys):
    import subprocess as sp

    import verify_tray_quit as vtq

    stops = []
    payload = {"windows": [{"title": "Herdr-Nachtwächter - Live-Status"}],
               "processes": [{"ProcessId": 111, "CommandLine": "herdr-nachtwaechter.exe"}]}
    calls = {"dump": 0}

    def fake_run(argv, **kwargs):
        if argv[0] == "powershell.exe":
            stops.append(argv)
        return sp.CompletedProcess(argv, 0, "", "")

    def dump_once_then_times_out():
        calls["dump"] += 1
        if calls["dump"] == 1:
            return payload
        raise sp.TimeoutExpired(["python.exe"], 15)

    monkeypatch.setattr(vtq.subprocess, "run", fake_run)
    monkeypatch.setattr(vtq, "dump", dump_once_then_times_out)
    monkeypatch.setattr(vtq.time, "sleep", lambda _s: None)
    assert vtq.main() == 3
    out = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert out["reason"] == "dump_failed" and out["stage"] == "after_stop"
    assert out["stop_pids"] == [111] and out["remaining_state"] == "unknown"
    assert len(stops) == 1
