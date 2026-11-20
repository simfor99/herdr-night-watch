#!/usr/bin/env python3
"""AGY-Multi-Konto-Quota-Fetcher für den Herdr-Nachtwächter.

Vertrag für src/quota.rs (Rust-Seite identisch zu bauer-2): genau ein JSON-Dokument
auf stdout, bei jedem Fehler `null` mit Exit 0 (fail-closed, kein Flackern).

Das Skript wird per include_str! eingebettet und als `python3 -c` ausgeführt; darum
gibt es kein `__file__` als Pflicht und einen festen Fallback auf das Repo-Tools-Verzeichnis.
"""

import calendar
import json
import os
import subprocess
import sys
import time

sys.dont_write_bytecode = True


def _repo_tools_dir():
    """Directory that holds llm_account.py. The embedded copy (python3 -c) has no __file__."""
    if "__file__" in globals():
        return os.path.dirname(os.path.abspath(__file__))
    override = os.environ.get("HERDR_NIGHT_WATCH_TOOLS")
    if override:
        return override
    return os.path.join(os.path.expanduser("~"), "projects", "herdr-night-watch", "tools")


REPO_TOOLS = _repo_tools_dir()
FALLBACK_TIMEOUT_S = 12


def _import_llm_account():
    candidates = []
    if "__file__" in globals():
        candidates.append(os.path.dirname(os.path.abspath(__file__)))
    candidates.append(REPO_TOOLS)
    for directory in candidates:
        if os.path.isfile(os.path.join(directory, "llm_account.py")):
            if directory not in sys.path:
                sys.path.insert(0, directory)
            import llm_account  # noqa: PLC0415  (bewusst lazy: fehlt die Datei, greift der Fallback)

            return llm_account
    raise ImportError("llm_account.py nicht gefunden")


def _epoch(iso_value):
    if not isinstance(iso_value, str) or not iso_value:
        return None
    try:
        return int(calendar.timegm(time.strptime(iso_value, "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return None


AUTH_ERROR_TEXT = {
    "relogin_required": "Login erneuern",
    "unavailable": "Abruf fehlgeschlagen",
}


def _auth_error(auth_state):
    """Klartext für den Tooltip; None bei gesundem Konto."""
    if auth_state == "ok":
        return None
    return AUTH_ERROR_TEXT.get(auth_state, "Abruf fehlgeschlagen")


def _project_registry_snapshot(snapshot):
    provider = snapshot["providers"]["agy"]
    lane = provider["primary_lane"]
    eff = provider["effective"]["lanes"][lane]
    accounts = []
    for acc in provider["accounts"]:
        gem = acc["lanes"][lane]
        accounts.append({
            "id": acc["id"],
            "label": acc["label"],
            "email": acc["email"],
            "is_active": acc["is_active"],
            "auth_state": acc["auth_state"],
            "auth_error": _auth_error(acc["auth_state"]),
            "five_hour_pct": gem["five_hour_pct"],
            "five_hour_reset": gem["five_hour_reset"],
            "week_pct": gem["weekly_pct"],
            "week_reset": gem["weekly_reset"],
        })
    pooled_count = eff["pooled_count"]
    refill = _epoch(eff["next_refill_at"]) if pooled_count >= 2 else None
    return {
        "version": 1,
        "active_account": provider["active_account"],
        "pooled_count": pooled_count,
        "pool_next_refill_epoch": refill,
        "accounts": accounts,
    }


def _has_active_slots(llm):
    reg = llm.load_registry()
    block = reg.get("providers", {}).get("agy", {})
    return any(s.get("state") == llm.ACTIVE for s in block.get("slots", []))


def _registry_result(llm):
    try:
        snapshot = llm.fetch_provider_status(llm.ADAPTERS["agy"], write_quotas=True)
        if snapshot.get("registry_changed"):
            return None  # überholt: ein paralleler Wechsel hat die Auswertung entwertet
        return _project_registry_snapshot(snapshot)
    except Exception:  # noqa: BLE001 - fail-closed: jeder Fehler wird zu null
        return None


def _fallback_result(llm):
    """Ohne accounts.json: direkter agy-Aufruf im Live-Home, genau ein Konto."""
    if llm is None:
        # Ohne Modul ist der TSV-Parser nicht verfügbar; ein agy-Lauf wäre verworfen. Also gar nicht erst starten.
        return None
    try:
        agy_bin = llm.agy_binary()  # prüft AGY_REAL_BIN fail-closed
    except Exception:  # noqa: BLE001
        return None
    env = dict(os.environ, HOME=str(llm.live_home()), BROWSER="false", DISPLAY="", XDG_OPEN="")
    try:
        proc = subprocess.run(
            [agy_bin, "-p", "/usage"],
            env=env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=FALLBACK_TIMEOUT_S,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0:
        return None
    adapter = llm.ADAPTERS["agy"]
    gem = llm.parse_usage_tsv(proc.stdout, adapter).get(adapter.primary_lane)
    if not gem or gem["weekly_pct"] is None:
        return None
    email = llm.read_id_email(llm.live_home() / adapter.token_rel)
    return {
        "version": 1,
        "active_account": None,
        "pooled_count": 1,
        "pool_next_refill_epoch": None,
        "accounts": [{
            "id": None,
            "label": None,
            "email": email,
            "is_active": True,
            "auth_state": "ok",
            "auth_error": None,
            "five_hour_pct": gem["five_hour_pct"],
            "five_hour_reset": gem["five_hour_reset"],
            "week_pct": gem["weekly_pct"],
            "week_reset": gem["weekly_reset"],
        }],
    }


def main():
    try:
        llm = _import_llm_account()
    except Exception:  # noqa: BLE001
        llm = None

    result = None
    if llm is not None:
        try:
            multi = _has_active_slots(llm)
        except Exception:  # noqa: BLE001
            # Kaputte accounts.json: kein Rückfall auf ein Konto, sonst würde ein Mehrkonto-Pool still verkürzt.
            sys.stdout.write("null\n")
            return 0
        if multi:
            result = _registry_result(llm)
            sys.stdout.write(json.dumps(result, ensure_ascii=False) + "\n" if result else "null\n")
            return 0
    result = _fallback_result(llm)
    sys.stdout.write(json.dumps(result, ensure_ascii=False) + "\n" if result else "null\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
