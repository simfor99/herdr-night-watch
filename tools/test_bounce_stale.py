"""Tests für `llm-account bounce-stale`. Vollständig mit Fake-Herdr und Fake-/proc; kein realer Eingriff in Panes."""

import json
import os
import shlex
import shutil
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

TOOLS = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, TOOLS)

import llm_account as la  # noqa: E402
from test_llm_account import env  # noqa: E402,F401  (Fixture)

CONV_OLD = "11111111-2222-4333-8444-555555555555"
CONV_OTHER = "66666666-7777-4888-8999-aaaaaaaaaaaa"
MODEL = "gemini-4-hydrogen-high"
BTIME = 1_795_000_000
SWITCH_EPOCH = BTIME + 1000
TICKS = os.sysconf("SC_CLK_TCK")


EMPTY_SCREEN = "─" * 40 + "\n>\n" + "─" * 40 + "\nauto · Gemini 4 Neon · high\n"


class FakeHerdr:
    def __init__(self, panes, on_run=None, *, tab="bauer", screen=EMPTY_SCREEN, foreground=("zsh",), marker=None):
        self.panes = panes
        self.calls = []
        self.keys = []
        self.on_run = on_run
        self.tab = tab
        self.screen = screen
        self.foreground = list(foreground)
        # existiert die Datei, läuft die Pane (Fake für "working" nach dem Weckruf)
        self.marker = marker if marker is not None else (
            Path(os.environ["FAKE_CHAT_MARKER"]) if os.environ.get("FAKE_CHAT_MARKER") else None
        )

    def pane_list(self):
        if self.marker is not None and self.marker.exists():
            return [{**p, "agent_status": "working"} for p in self.panes]
        return self.panes

    def send_keys(self, pane_id, key):
        self.keys.append((pane_id, key))

    def tab_label(self, tab_id):
        return self.tab

    def screen_text(self, pane_id):
        return self.screen

    def foreground_names(self, pane_id):
        return list(self.foreground)

    def run(self, pane_id, text):
        self.calls.append((pane_id, text))
        if self.on_run:
            self.on_run(self, pane_id, text)


FAKE_CHAT = """#!/bin/sh
for a in "$@"; do printf '%s\\0' "$a"; done >> "$FAKE_CHAT_LOG"
if [ -n "$FAKE_CHAT_FAIL" ]; then exit 1; fi
if [ -z "$FAKE_CHAT_NO_WORK" ]; then touch "$FAKE_CHAT_MARKER"; fi
exit 0
"""


@pytest.fixture(autouse=True)
def fake_chat(tmp_path, monkeypatch):
    """Ersetzt herdr_chat.py durch ein Skript, das seine Argumente protokolliert. Kein realer Versand."""
    script = tmp_path / "fake_chat.sh"
    script.write_text(FAKE_CHAT, encoding="utf-8")
    script.chmod(0o755)
    log = tmp_path / "chat.log"
    marker = tmp_path / "working.marker"
    monkeypatch.setenv("HERDR_CHAT_BIN", str(script))
    monkeypatch.setenv("FAKE_CHAT_LOG", str(log))
    monkeypatch.setenv("FAKE_CHAT_MARKER", str(marker))
    monkeypatch.delenv("FAKE_CHAT_FAIL", raising=False)
    monkeypatch.delenv("FAKE_CHAT_NO_WORK", raising=False)
    return SimpleNamespace(log=log, marker=marker)


def chat_args(fake):
    """Argumente des (einzigen) protokollierten herdr_chat-Aufrufs; leer, wenn nichts gesendet wurde."""
    if not fake.log.exists():
        return []
    return [arg for arg in fake.log.read_text(encoding="utf-8").split("\0") if arg]


@pytest.fixture
def world(env, tmp_path, monkeypatch):
    """Registry mit switch_epoch, /proc-Fake mit einem veralteten agy in pane w1:p1, Live-Home mit Konversation."""
    proc = tmp_path / "proc"
    proc.mkdir()
    (proc / "stat").write_text(f"cpu 1 2 3\nbtime {BTIME}\n", encoding="utf-8")
    monkeypatch.setenv("LLM_ACCOUNTS_PROC", str(proc))
    env.state.mkdir(parents=True, exist_ok=True)
    (env.state / "accounts.json").write_text(json.dumps({
        "version": 1,
        "providers": {"agy": {"active_account": None, "switch_epoch": SWITCH_EPOCH, "slots": []}},
    }), encoding="utf-8")
    # Konversation klein halten
    brain = env.live / ".gemini/antigravity-cli/brain" / CONV_OLD
    brain.mkdir(parents=True)
    (brain / "transcript.jsonl").write_text("x" * 1000, encoding="utf-8")
    (env.live / ".gemini/antigravity-cli/conversations").mkdir(parents=True)
    (env.live / ".gemini/antigravity-cli/conversations" / f"{CONV_OLD}.pb").write_bytes(b"y" * 100)
    # Ein echtes Arbeitsverzeichnis: der Neustart nach /exit braucht es (cwd_ungueltig sonst).
    work = tmp_path / "work"
    work.mkdir(exist_ok=True)
    return SimpleNamespace(proc=proc, env=env, cwd=str(work))


def add_agy(proc, pid, pane, *, conv=CONV_OLD, model=MODEL, start_epoch=BTIME + 500, cwd=None):
    entry = proc / str(pid)
    entry.mkdir()
    argv = ["/usr/local/bin/agy"]
    if conv:
        argv += ["--conversation", conv]
    if model:
        argv += ["--model", model]
    (entry / "cmdline").write_bytes(b"\0".join(a.encode() for a in argv) + b"\0")
    after = ["S"] + ["0"] * 18 + [str((start_epoch - BTIME) * TICKS)] + ["0"] * 5
    (entry / "stat").write_text(f"{pid} (agy) " + " ".join(after) + "\n", encoding="utf-8")
    (entry / "environ").write_bytes(f"HERDR_PANE_ID={pane}\0HERDR_TAB_ID=t1\0".encode())
    if cwd:
        os.symlink(cwd, entry / "cwd")


def pane(status="idle", agent="agy", conv=CONV_OLD, pane_id="w1:p1"):
    session = {"agent": "agy", "kind": "id", "value": conv} if conv else None
    # focused ist ein echtes Herdr-Feld; fehlt es, sperrt der Guard (fail-closed).
    entry = {"pane_id": pane_id, "agent": agent, "agent_status": status, "tab_id": "w1:t1", "cwd": "/x",
             "focused": False}
    if session:
        entry["agent_session"] = session
    return entry


def test_dry_run_reports_the_plan_and_touches_nothing(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])

    out = la.cmd_bounce_stale(
        la.ADAPTERS["agy"],
        SimpleNamespace(pane_id="w1:p1", dry_run=True, yes=False, conversation=None, cwd=None, model=None, force_pane=False),
        herdr=herdr,
    )

    plan = json.loads(out)
    assert plan["dry_run"] is True
    assert plan["action"] == "in_place_resume"
    assert plan["recommended_action"] == "in_place_resume"
    assert plan["resume_cmd"] == ["agy", "--conversation", CONV_OLD, "--model", MODEL]
    assert plan["conversation_id"] == CONV_OLD
    assert plan["db_bytes"] == 100
    assert plan["brain_bytes"] == 1000
    assert plan["cwd"] == world.cwd
    assert plan["pid"] == 4242
    assert plan["stale"] is True
    assert plan["blockers"] == []
    assert herdr.calls == []


def test_dry_run_writes_nothing_and_only_previews_heal(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    before = sorted(p.name for p in world.env.state.iterdir())

    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), proc=world.proc)

    assert plan["heal_preview"] == "no_live_token"
    assert sorted(p.name for p in world.env.state.iterdir()) == before
    assert not (world.env.state / "events.jsonl").exists()


def test_live_action_requires_yes(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])

    with pytest.raises(la.CliError) as info:
        la.cmd_bounce_stale(
            la.ADAPTERS["agy"],
            SimpleNamespace(pane_id="w1:p1", dry_run=False, yes=False, conversation=None, cwd=None, model=None, force_pane=False),
            herdr=herdr,
        )
    assert info.value.exit_code == 2
    assert herdr.calls == []


def test_refuses_a_pane_that_does_not_run_agy(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    with pytest.raises(la.CliError):
        la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane(agent="claude")]), proc=world.proc)


def test_refuses_a_process_that_is_not_stale_unless_forced(world):
    add_agy(world.proc, 4242, "w1:p1", start_epoch=SWITCH_EPOCH + 60, cwd=world.cwd)
    herdr = FakeHerdr([pane()])

    with pytest.raises(la.CliError):
        la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc, force_pane=True)
    assert plan["stale"] is False and plan["force_pane"] is True


def test_refuses_a_conversation_that_differs_from_the_running_process(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    with pytest.raises(la.CliError):
        la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), conversation=CONV_OTHER, proc=world.proc)


def test_pane_session_and_process_disagreement_routes_to_mutter_kind(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane(conv=CONV_OTHER)]), proc=world.proc)
    assert plan["recommended_action"] == "herdr-mutter-kind"
    assert plan["resume_cmd"] is None
    assert any("weichen" in b for b in plan["blockers"])


def test_missing_model_routes_to_mutter_kind_instead_of_guessing(world):
    add_agy(world.proc, 4242, "w1:p1", model=None, cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), proc=world.proc)
    assert plan["recommended_action"] == "herdr-mutter-kind"
    assert plan["model"] is None and plan["to_model"] is None
    assert any("Modell" in b for b in plan["blockers"])


def test_missing_conversation_id_routes_to_mutter_kind(world):
    add_agy(world.proc, 4242, "w1:p1", conv=None, model=MODEL, cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane(conv=None)]), proc=world.proc)
    assert plan["recommended_action"] == "herdr-mutter-kind"
    assert plan["conversation_id"] is None
    assert any("conversation_id" in b for b in plan["blockers"])


def test_large_conversation_is_routed_to_herdr_mutter_kind_and_never_bounced(world, monkeypatch):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    monkeypatch.setattr(la, "BOUNCE_MAX_BYTES", 10)
    herdr = FakeHerdr([pane()])

    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    assert plan["recommended_action"] == "herdr-mutter-kind"
    assert plan["to_model"] == MODEL
    assert any("Schwelle" in b for b in plan["blockers"])

    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc)
    assert result["status"] == "refused"
    assert result["recommended_action"] == "herdr-mutter-kind"
    assert herdr.calls == []


def test_successful_bounce_logs_a_pane_bounced_event(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2, poll_s=0.01)

    bounced = [json.loads(line) for line in (world.env.state / "events.jsonl").read_text().splitlines()]
    bounced = [e for e in bounced if e["event"] == "pane_bounced"]
    assert len(bounced) == 1
    assert bounced[0]["old_pid"] == 4242 and bounced[0]["new_pid"] == 5151
    assert bounced[0]["status"] == "ok" and bounced[0]["conversation_id"] == CONV_OLD


def test_leftover_agy_on_the_pane_fails_closed_before_restart(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
            add_agy(world.proc, 4343, "w1:p1", conv=CONV_OTHER, cwd=world.cwd)  # zweiter Prozess auf derselben Pane

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, poll_s=0.01)

    assert result["status"] == "failed" and result["stage"] == "exit"
    assert result["leftover_pids"] == [4343]
    assert [c[1] for c in herdr.calls] == ["/exit"]


def test_live_bounce_exits_restarts_same_conversation_in_same_cwd(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")  # alter Prozess ist beendet
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)

    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2, poll_s=0.01)

    assert result["status"] == "ok"
    assert result["new_pid"] == 5151
    assert result["heal"] in {"no_live_token", "no_active", "current", "synced", "clobber_recovered"}
    assert [c[1] for c in herdr.calls][0] == "/exit"
    # Der Neustartbefehl wird nach dem Shell-Parsing verglichen, nicht als Rohstring: der pytest-Temppfad kann Leerzeichen enthalten.
    assert shlex.split(herdr.calls[1][1]) == ["cd", world.cwd, "&&", "agy", "--conversation", CONV_OLD, "--model", MODEL]


def test_failing_exit_is_reported_without_starting_a_second_process(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])  # /exit wirkt nicht
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)

    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=0.1, poll_s=0.01)

    assert result["status"] == "failed" and result["stage"] == "exit"
    assert result["recommended_action"] == "herdr-mutter-kind"
    assert [c[1] for c in herdr.calls] == ["/exit"]


def test_failed_restart_recommends_herdr_mutter_kind_with_the_original_model(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)

    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, start_timeout_s=0.1, poll_s=0.01)

    assert result["status"] == "failed" and result["stage"] == "start"
    assert result["recommended_action"] == "herdr-mutter-kind"
    assert result["to_model"] == MODEL


def test_busy_pane_is_never_touched(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane(status="working")])
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)

    with pytest.raises(la.CliError):
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc)
    assert herdr.calls == []


def test_model_with_shell_metacharacters_is_rejected_before_any_command(world):
    add_agy(world.proc, 4242, "w1:p1", model="gemini-4;rm -rf x", cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), proc=world.proc)
    assert plan["recommended_action"] == "herdr-mutter-kind"
    assert any("unzulässige Zeichen" in b for b in plan["blockers"])
    assert plan["resume_cmd"] is None


def test_restart_command_quotes_cwd_and_model(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    odd = Path(world.cwd).parent / "dir with space"  # existierendes Verzeichnis: ungültige cwd wird abgelehnt
    odd.mkdir(exist_ok=True)
    odd_cwd = str(odd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    plan["cwd"] = odd_cwd
    la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2, poll_s=0.01)

    restart = herdr.calls[-1][1]
    assert restart == f"cd '{odd_cwd}' && agy --conversation {CONV_OLD} --model {MODEL}"


def test_scan_reads_model_from_short_flag_m_as_well(world):
    entry = world.proc / "4444"
    entry.mkdir()
    argv = [b"/usr/local/bin/agy", b"--conversation", CONV_OLD.encode(), b"-m", MODEL.encode()]
    (entry / "cmdline").write_bytes(b"\0".join(argv) + b"\0")
    after = ["S"] + ["0"] * 18 + [str(600 * TICKS)] + ["0"] * 5
    (entry / "stat").write_text("4444 (agy) " + " ".join(after) + "\n", encoding="utf-8")
    (entry / "environ").write_bytes(b"HERDR_PANE_ID=w1:p9\0")

    rec = la.scan_agy_processes(world.proc)[0]
    assert rec["model"] == MODEL
    assert rec["pane_id"] == "w1:p9"


def test_protected_human_tabs_are_flagged_but_ordinary_tabs_are_not():
    assert la.protected_tab("design studio")
    assert la.protected_tab("architekt-2")
    assert la.protected_tab("🏗 architekt")
    assert la.protected_tab("senior-architekt")
    assert la.protected_tab("🔥 eskalations-architekt")
    assert la.protected_tab("vermarkter")
    assert not la.protected_tab("🔨 bauer")
    assert not la.protected_tab("Bau")
    assert not la.protected_tab(None)


def test_plan_reports_tab_label_and_protection(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()], tab="design studio"), proc=world.proc)
    assert plan["tab_label"] == "design studio"
    assert plan["protected_human_tab"] is True
    # AUTH-HEAL-1a Bedingung 1: menschliche Tabs sind immer gesperrt. Die frühere reine Ausweisung
    # ohne Blocker widersprach dieser Regel und wurde deshalb auf einen festen Blocker umgestellt.
    assert "protected_human_tab" in plan["blockers"]
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_composer_draft_is_detected_and_blocks_before_exit(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    draft_screen = "─" * 40 + "\n> bitte noch nicht senden\n" + "─" * 40 + "\n"
    herdr = FakeHerdr([pane()], screen=draft_screen)

    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    assert plan["composer_draft"] is True
    assert plan["live_blockers"]

    with pytest.raises(la.CliError) as info:
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc)
    assert info.value.exit_code == 4
    assert herdr.calls == []


def test_unrecognisable_composer_fails_closed(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()], screen="Bildschirm ohne Composer\n")
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    assert plan["composer_draft"] is None

    with pytest.raises(la.CliError):
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc)
    assert herdr.calls == []


@pytest.mark.parametrize("screen,expected", [
    ("─" * 10 + "\n>\n" + "─" * 10, False),
    ("─" * 10 + "\n>   \n" + "─" * 10, False),
    ("─" * 10 + "\n> offener Entwurf\n" + "─" * 10, True),
    ("kein Composer", None),
])
def test_composer_draft_classification(screen, expected):
    assert la.composer_draft(screen) is expected


def test_no_second_exit_when_shell_is_not_foreground_after_exit(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")

    herdr = FakeHerdr([pane()], on_run=on_run, foreground=("agy",))  # Sub-TUI bleibt im Vordergrund
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, shell_timeout_s=0.05, poll_s=0.01)

    assert result["status"] == "failed" and result["stage"] == "shell"
    assert result["foreground"] == ["agy"]
    assert [c[1] for c in herdr.calls] == ["/exit"]


def test_only_the_db_counts_against_the_threshold_not_the_brain(world, monkeypatch):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    # brain/ ist 1000 Byte, .pb 100 Byte: Schwelle 500 Byte trifft nur die brain-Summe, nicht die DB.
    monkeypatch.setattr(la, "BOUNCE_MAX_BYTES", 500)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), proc=world.proc)
    assert plan["brain_bytes"] == 1000 > 500
    assert plan["recommended_action"] == "in_place_resume"

    monkeypatch.setattr(la, "BOUNCE_MAX_BYTES", 50)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()]), proc=world.proc)
    assert plan["db_bytes"] == 100 > 50
    assert plan["recommended_action"] == "herdr-mutter-kind"


def test_default_sends_the_resume_prompt_through_herdr_chat_and_documents_it(world, fake_chat):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, work_timeout_s=2)

    assert result["wakeup_sent"] is True
    assert result["wakeup_status"] == "working_seen"
    args = chat_args(fake_chat)
    assert args[0] == "send"  # das Fake-Skript protokolliert $@ ohne den eigenen Pfad
    assert args[args.index("send") + 1] == "--durable"
    assert args[args.index("--art") + 1] == "nachricht"
    assert args[args.index("--betreff") + 1] == la.WAKEUP_BETREFF
    assert args[-2] == "w1:p1"
    assert args[-1] == la.WAKEUP_TEXT
    bounced = [json.loads(l) for l in (world.env.state / "events.jsonl").read_text().splitlines()]
    event = [e for e in bounced if e["event"] == "pane_bounced"][0]
    assert event["wakeup_sent"] is True and event["wakeup_status"] == "working_seen"


def test_no_resume_prompt_skips_the_wakeup_entirely(world, fake_chat):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, resume_prompt=False)

    assert result["status"] == "ok"
    assert result["wakeup_sent"] is False and result["wakeup_status"] == "disabled"
    assert chat_args(fake_chat) == []


def test_failing_herdr_chat_does_not_undo_the_bounce_but_reports_it(world, fake_chat, monkeypatch):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    monkeypatch.setenv("FAKE_CHAT_FAIL", "1")

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, work_timeout_s=0.1)

    assert result["status"] == "ok"
    assert result["wakeup_sent"] is False
    assert result["wakeup_status"].startswith("send_failed")


def test_no_working_state_after_send_is_reported_not_faked(world, fake_chat, monkeypatch):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    monkeypatch.setenv("FAKE_CHAT_NO_WORK", "1")

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, work_timeout_s=0.1)

    assert result["wakeup_sent"] is True
    assert result["wakeup_status"] == "no_working_seen"


def test_esc_banner_is_dismissed_before_the_wakeup(world, fake_chat):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    banner_screen = EMPTY_SCREEN + "  esc to dismiss\n"

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane()], on_run=on_run, screen=banner_screen)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, work_timeout_s=0.1)

    assert herdr.keys == [("w1:p1", "esc")]
    assert result["wakeup_sent"] is True


def test_wakeup_never_sends_into_a_busy_pane(world, fake_chat):
    """Der Weckruf selbst sendet nur an eine bereite Pane; das Gate vor /exit ist separat getestet."""
    herdr = FakeHerdr([pane(status="working")])
    outcome = la._wakeup(herdr, "w1:p1", ready_timeout_s=0.1, work_timeout_s=0.1, poll_s=0.01)

    assert outcome == {"wakeup_sent": False, "wakeup_status": "pane_not_ready"}
    assert chat_args(fake_chat) == []



def test_done_pane_runs_the_bounce_and_the_wakeup_through(world, fake_chat):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    herdr = FakeHerdr([pane(status="done")], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    assert plan["live_blockers"] == []
    assert plan["recommended_action"] == "in_place_resume"

    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2, start_timeout_s=2,
                               poll_s=0.01, work_timeout_s=2)

    assert result["status"] == "ok"
    assert result["new_pid"] == 5151
    assert result["wakeup_sent"] is True and result["wakeup_status"] == "working_seen"
    assert chat_args(fake_chat)[0] == "send"


def test_working_pane_is_already_blocked_in_the_dry_run(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    out = la.cmd_bounce_stale(
        la.ADAPTERS["agy"],
        SimpleNamespace(pane_id="w1:p1", dry_run=True, yes=False, conversation=None, cwd=None, model=None, force_pane=False),
        herdr=FakeHerdr([pane(status="working")]),
    )
    plan = json.loads(out)
    assert any("Pane nicht bereit" in b and "working" in b for b in plan["live_blockers"])


@pytest.mark.parametrize("status", ["working", "blocked", "unknown"])
def test_dry_run_and_yes_agree_on_non_ready_panes(world, status):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane(status=status)])
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    assert plan["live_blockers"]  # Dry-Run weist es aus

    with pytest.raises(la.CliError):  # --yes verweigert denselben Zustand
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc)
    assert herdr.calls == []


# ---- CodeRabbit-Befunde (Folgecommit): Fokus, Prozessbindung, cwd, Tab-Schutz, Weckruf ----


def test_focus_taken_during_the_quiet_period_blocks_the_exit(world, monkeypatch):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    original = la._quiet_snapshot

    def focus_during_quiet(h, pane_id, root, pid):
        result = original(h, pane_id, root, pid)
        h.panes[0]["focused"] = True  # der Nutzer fokussiert die Pane während der Ruhephase
        return result

    monkeypatch.setattr(la, "_quiet_snapshot", focus_during_quiet)
    with pytest.raises(la.CliError, match="pane_focused"):
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=0.1, poll_s=0.01)
    assert herdr.calls == []


def test_another_process_in_the_same_pane_before_exit_is_refused(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    herdr = FakeHerdr([pane()])
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    shutil.rmtree(world.proc / "4242")
    add_agy(world.proc, 4343, "w1:p1", conv=CONV_OTHER, cwd=world.cwd)  # anderer Prozess, andere Konversation

    with pytest.raises(la.CliError, match="prozessbindung_geaendert"):
        la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=0.1, poll_s=0.01)
    assert herdr.calls == []


def test_a_working_directory_that_does_not_exist_is_refused_before_exit(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=str(Path(world.cwd) / "gone"))
    herdr = FakeHerdr([pane()])
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)

    assert "cwd_ungueltig" in plan["blockers"]
    assert plan["action"] is None
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=0.1, poll_s=0.01)
    assert result["status"] == "refused"
    assert herdr.calls == []


def test_unreadable_tab_label_is_an_unknown_protection_and_blocks(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", FakeHerdr([pane()], tab=None), proc=world.proc)
    assert "tab_schutz_unbekannt" in plan["blockers"]
    assert plan["action"] is None


def test_wakeup_observation_failure_keeps_the_new_pid_and_reports_it_separately(world):
    add_agy(world.proc, 4242, "w1:p1", cwd=world.cwd)

    def on_run(fake, pane_id, text):
        if text == "/exit":
            shutil.rmtree(world.proc / "4242")
        elif text.startswith("cd "):
            add_agy(world.proc, 5151, "w1:p1", cwd=world.cwd)

    class ObservationLost(FakeHerdr):
        def pane_list(self):
            if self.marker is not None and self.marker.exists():  # nach dem Weckruf fällt herdr aus
                raise la.CliError("herdr pane list nicht erreichbar", 5)
            return super().pane_list()

    herdr = ObservationLost([pane()], on_run=on_run)
    plan = la.plan_bounce(la.ADAPTERS["agy"], "w1:p1", herdr, proc=world.proc)
    result = la.execute_bounce(la.ADAPTERS["agy"], plan, herdr, proc=world.proc, exit_timeout_s=2,
                               start_timeout_s=2, poll_s=0.01)

    assert result["status"] == "ok" and result["new_pid"] == 5151
    assert result["wakeup_sent"] is True
    assert result["wakeup_status"].startswith("beobachtung_fehlgeschlagen")
