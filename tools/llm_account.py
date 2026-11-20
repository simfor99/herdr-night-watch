#!/usr/bin/env python3
"""llm-account: provider-agnostische Kontenverwaltung (N >= 1) mit dünnem agy-Adapter.

Kanonischer Aufruf: llm-account <verb> <provider> [slot]

Geheimnisse (OAuth-Token) verlassen diese Datei nie über stdout, Events oder
Snapshots. Alle Schreibvorgänge auf Credentials sind atomar (mkstemp, chmod 0600,
fsync, os.replace). Isolierte Aufrufe von agy laufen ausnahmslos über den
echten Binary-Pfad (AGY_REAL_BIN), niemals über den $PATH-Wrapper.
"""

from __future__ import annotations

import argparse
import calendar
import concurrent.futures
import contextlib
import copy
import fcntl
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterator

ACTIVE = "active"
RETIRED = "retired"
AUTH_OK = "ok"
AUTH_RELOGIN = "relogin_required"
AUTH_UNAVAILABLE = "unavailable"

FLOOR_PCT = 15
FEFO_FIVE_HOUR_MIN_PCT = 10
USAGE_TIMEOUT_S = 12
DEFAULT_AGY_BIN = str(Path.home() / ".local" / "bin" / "agy")
DEFAULT_STATE_DIR = "~/.local/state/llm-accounts"
DEFAULT_LIVE_HOME = "~"
DEFAULT_PROC_ROOT = "/proc"
DEFAULT_UNIT_DIR = "~/.config/systemd/user"
SLOT_RE = re.compile(r"[a-z0-9][a-z0-9_-]{0,31}")
DAY_S = 86400.0


class CliError(Exception):
    def __init__(self, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.exit_code = exit_code


# ---------------------------------------------------------------------------
# Pfade und Umgebung (zur Laufzeit gelesen, damit Tests sie überschreiben können)
# ---------------------------------------------------------------------------


def state_dir() -> Path:
    return Path(os.environ.get("LLM_ACCOUNTS_DIR") or DEFAULT_STATE_DIR).expanduser()


def live_home() -> Path:
    return Path(os.environ.get("LLM_ACCOUNTS_HOME") or DEFAULT_LIVE_HOME).expanduser()


def proc_root() -> Path:
    return Path(os.environ.get("LLM_ACCOUNTS_PROC") or DEFAULT_PROC_ROOT)


def agy_binary() -> str:
    validate_agy_binary_override()
    return os.environ.get("AGY_REAL_BIN") or DEFAULT_AGY_BIN


def validate_agy_binary_override() -> None:
    """AGY_REAL_BIN muss ein absoluter Pfad sein. Ein nackter Name liefe über $PATH und könnte den Wrapper treffen."""
    value = os.environ.get("AGY_REAL_BIN")
    if value is not None and not os.path.isabs(value):
        raise CliError(f"AGY_REAL_BIN muss ein absoluter Pfad sein, erhalten: {value!r}.", 2)


def utc_iso(epoch: float | None = None) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch if epoch is not None else time.time()))


def iso_to_epoch(value: Any) -> float | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return float(calendar.timegm(time.strptime(value, "%Y-%m-%dT%H:%M:%SZ")))
    except ValueError:
        return None


# ---------------------------------------------------------------------------
# Provider-Adapter
# ---------------------------------------------------------------------------


class AgyAdapter:
    """Dünner Adapter: genau zwei 0600-Dateien tauschen das Konto; Onboarding wird für Profile geseedet."""

    name = "agy"
    lanes = ("gemini", "claude_gpt")
    primary_lane = "gemini"
    token_rel = Path(".gemini/antigravity-cli/antigravity-oauth-token")
    scope_rel = Path(".config/antigravity/credential-user-scope")
    onboarding_rel = Path(".gemini/antigravity-cli/cache/onboarding.json")
    swap_rels = (token_rel, scope_rel)
    profile_rels = (token_rel, scope_rel, onboarding_rel)
    onboarding_seed = {
        "consumerOnboardingComplete": True,
        "enterpriseOnboardingComplete": False,
        "onboardingComplete": True,
    }
    lane_markers = (("Gemini", "gemini"), ("Claude", "claude_gpt"))
    auth_markers = ("login", "sign in", "unauthorized", "authenticat", "anmeld")

    def usage_argv(self) -> list[str]:
        return [agy_binary(), "-p", "/usage"]


ADAPTERS: dict[str, AgyAdapter] = {"agy": AgyAdapter()}


def get_adapter(name: str) -> AgyAdapter:
    adapter = ADAPTERS.get(name)
    if adapter is None:
        raise CliError(f"Unbekannter Provider {name!r} (heute nur: {', '.join(ADAPTERS)}).", 2)
    return adapter


def validate_slot(slot: str) -> str:
    if not SLOT_RE.fullmatch(slot or ""):
        raise CliError(f"Ungültige Slot-ID {slot!r}: erlaubt sind Kleinbuchstaben, Ziffern, '-' und '_'.", 2)
    return slot


def profile_root(adapter: AgyAdapter, slot: str) -> Path:
    return state_dir() / "profiles" / adapter.name / slot


# ---------------------------------------------------------------------------
# Dateisystem: atomare Schreibvorgänge mit Eigentümer-Rechten (0600/0700)
# ---------------------------------------------------------------------------


def ensure_state() -> Path:
    root = state_dir()
    root.mkdir(parents=True, exist_ok=True)
    os.chmod(root, 0o700)
    return root


def ensure_owner_only_dir(path: Path) -> Path:
    root = ensure_state()
    cur = root
    for part in path.relative_to(root).parts:
        cur = cur / part
        cur.mkdir(exist_ok=True)
        os.chmod(cur, 0o700)
    return path


def _fsync_dir(directory: Path) -> None:
    with contextlib.suppress(OSError):
        fd = os.open(directory, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def atomic_write(path: Path, data: bytes, *, mode: int = 0o600, owner_only: bool = False) -> None:
    if owner_only:
        ensure_owner_only_dir(path.parent)
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=str(path.parent), prefix=f".{path.name}.")
    try:
        with os.fdopen(fd, "wb") as fh:
            os.fchmod(fh.fileno(), mode)
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp_name, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp_name)
        raise
    _fsync_dir(path.parent)


def copy_files(src_root: Path, dst_root: Path, rels: tuple[Path, ...], *, owner_only: bool = False) -> None:
    payloads: list[tuple[Path, bytes]] = []
    for rel in rels:
        src = src_root / rel
        try:
            payloads.append((rel, src.read_bytes()))
        except OSError as exc:
            raise CliError(f"Quelldatei fehlt oder ist nicht lesbar: {src} ({exc.strerror})", 3) from exc
    for rel, data in payloads:
        atomic_write(dst_root / rel, data, owner_only=owner_only)


def copy_present(src_root: Path, dst_root: Path, rels: tuple[Path, ...], *, owner_only: bool = False) -> None:
    """Kopiert nur die Dateien, die im Quellverzeichnis existieren (z. B. optionaler Scope)."""
    copy_files(src_root, dst_root, tuple(rel for rel in rels if (src_root / rel).exists()), owner_only=owner_only)


def install_profile_to_live(adapter: "AgyAdapter", pdir: Path) -> None:
    """Profil -> Live-Home: Token immer; Scope nur, wenn das Profil einen hat. Sonst entfernen wir den
    evtl. fremden Scope im Live-Home, damit agy ihn beim nächsten Start sauber neu anlegt."""
    copy_files(pdir, live_home(), (adapter.token_rel,))
    if (pdir / adapter.scope_rel).exists():
        copy_files(pdir, live_home(), (adapter.scope_rel,))
    else:
        _unlink_if_present(live_home() / adapter.scope_rel)


def mirror_live_to_profile(adapter: "AgyAdapter", pdir: Path, token_bytes: bytes) -> None:
    """Live-Home -> Profil bei save: Token aus den bereits geprüften Bytes; Scope spiegeln, fehlende Datei entfernen."""
    atomic_write(pdir / adapter.token_rel, token_bytes, owner_only=True)
    if (live_home() / adapter.scope_rel).exists():
        copy_files(live_home(), pdir, (adapter.scope_rel,), owner_only=True)
    else:
        _unlink_if_present(pdir / adapter.scope_rel)


def log_event(event: str, **fields: Any) -> None:
    root = ensure_state()
    record = {"ts": utc_iso(), "event": event}
    record.update({k: v for k, v in fields.items() if v is not None})
    fd = os.open(root / "events.jsonl", os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, ensure_ascii=False) + "\n")


@contextlib.contextmanager
def switch_lock() -> Iterator[None]:
    root = ensure_state()
    fd = os.open(root / "switch.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


# ---------------------------------------------------------------------------
# Registry (accounts.json, keine Secrets)
# ---------------------------------------------------------------------------


def load_registry() -> dict[str, Any]:
    path = state_dir() / "accounts.json"
    if not path.exists():
        return {"version": 1, "providers": {}}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CliError(f"accounts.json ist nicht lesbar: {exc}", 3) from exc
    data.setdefault("providers", {})
    return data


def save_registry(reg: dict[str, Any]) -> None:
    payload = json.dumps(reg, indent=2, ensure_ascii=False) + "\n"
    atomic_write(state_dir() / "accounts.json", payload.encode("utf-8"), owner_only=True)


def provider_block(reg: dict[str, Any], adapter: AgyAdapter) -> dict[str, Any]:
    return reg["providers"].setdefault(
        adapter.name, {"active_account": None, "switch_epoch": None, "slots": []}
    )


def find_slot(block: dict[str, Any], slot_id: str) -> dict[str, Any] | None:
    return next((s for s in block["slots"] if s.get("id") == slot_id), None)


def slot_for_email(block: dict[str, Any], email: str) -> dict[str, Any] | None:
    return next((s for s in block["slots"] if s.get("email") == email), None)


def mark_active(block: dict[str, Any], slot_id: str) -> None:
    # Sekundengenau abgeschnitten würde ein Prozess, der kurz vor dem Wechsel startete, als neuer gelten.
    if block.get("active_account") != slot_id:
        block["active_account"] = slot_id
        block["switch_epoch"] = time.time()


def identity_from_bytes(raw: bytes) -> str | None:
    """E-Mail aus genau diesen Token-Bytes. Identität und Kopie müssen dieselben Bytes verwenden."""
    try:
        data = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    email = data.get("id_token_email") if isinstance(data, dict) else None
    return email if isinstance(email, str) and email else None


def read_id_email(token_path: Path) -> str | None:
    try:
        raw = token_path.read_bytes()
    except OSError:
        return None
    return identity_from_bytes(raw)


def invalidate_caches() -> list[str]:
    removed: list[str] = []
    quota_cache = live_home() / ".cache/quota-axi/quotas.json"
    if _unlink_if_present(quota_cache):
        removed.append("quota-axi/quotas.json")
    resolver_dir = live_home() / ".local/state/herdr/dispatch-resolve-cache"
    if resolver_dir.is_dir():
        for entry in resolver_dir.iterdir():
            if not entry.is_file() or entry.name == "quota-axi-refresh.state.json" or entry.name.endswith(".lock"):
                continue
            if _unlink_if_present(entry):
                removed.append(f"dispatch-resolve-cache/{entry.name}")
    return removed


def _unlink_if_present(path: Path) -> bool:
    """Entfernt eine Datei; verschwindet sie zwischen Prüfung und Löschen, ist das kein Fehler."""
    try:
        path.unlink()
    except FileNotFoundError:
        return False
    return True


# ---------------------------------------------------------------------------
# Heal: deterministischer Schutz gegen savingTokenSource-Clobbering
# ---------------------------------------------------------------------------


def heal_locked(adapter: AgyAdapter) -> str:
    """Muss unter switch_lock aufgerufen werden. Liefert den Ausgang als Kurzcode."""
    live_token = live_home() / adapter.token_rel
    if not live_token.exists():
        return "no_live_token"
    reg = load_registry()
    block = provider_block(reg, adapter)
    active_id = block.get("active_account")
    if not active_id:
        return "no_active"
    active = find_slot(block, active_id)
    if active is None:
        raise CliError(f"active_account {active_id!r} fehlt in den Slots der Registry.", 3)
    # Die Live-Datei wird genau einmal gelesen. Identität und Kopie stammen aus denselben Bytes, sonst könnte
    # ein Alt-Prozess zwischen Prüfung und Kopie ein fremdes Token in das Profil dieses Kontos schreiben.
    try:
        with live_token.open("rb") as fh:
            live_mtime = os.fstat(fh.fileno()).st_mtime  # Mtime und Bytes aus demselben Deskriptor
            token_bytes = fh.read()
    except OSError:
        return "unreadable"
    live_email = identity_from_bytes(token_bytes)
    if live_email is None:
        return "unreadable"

    pdir = profile_root(adapter, active_id)
    if live_email == active.get("email"):
        # Token nur bei neuerem Stand syncen; der Scope zählt, sobald er im Live-Home neu oder anders ist.
        profile_token = pdir / adapter.token_rel
        live_scope = live_home() / adapter.scope_rel
        token_newer = not profile_token.exists() or live_mtime > profile_token.stat().st_mtime
        scope_new = live_scope.exists() and (
            not (pdir / adapter.scope_rel).exists() or live_scope.read_bytes() != (pdir / adapter.scope_rel).read_bytes()
        )
        if token_newer:
            atomic_write(pdir / adapter.token_rel, token_bytes, owner_only=True)
        if scope_new:
            copy_files(live_home(), pdir, (adapter.scope_rel,), owner_only=True)
        return "synced" if (token_newer or scope_new) else "current"

    # Clobber: ein Alt-Prozess hat das Token eines anderen Kontos in die Live-Datei geschrieben.
    if not (pdir / adapter.token_rel).exists():
        raise CliError(f"Profil {active_id} hat kein Token; Wiederherstellung nicht möglich.", 3)
    origin = slot_for_email(block, live_email)
    rescued_to: str | None = None
    quarantined = False
    if origin is not None:
        # Nur das Token gehört zum Alt-Prozess. Der Scope im Live-Home gehört weiter zum aktiven Konto und
        # wird deshalb nie in den Ursprungs-Slot kopiert; ein dort fehlender Scope bleibt fehlend.
        origin_dir = profile_root(adapter, origin["id"])
        atomic_write(origin_dir / adapter.token_rel, token_bytes, owner_only=True)
        rescued_to = origin["id"]
    else:
        qdir = ensure_owner_only_dir(state_dir() / "quarantine" / f"{time.time_ns()}")
        atomic_write(qdir / adapter.token_rel, token_bytes, owner_only=True)
        copy_present(live_home(), qdir, tuple(r for r in adapter.swap_rels if r != adapter.token_rel), owner_only=True)
        quarantined = True
    install_profile_to_live(adapter, pdir)
    log_event(
        "clobber_recovered",
        provider=adapter.name,
        active=active_id,
        rescued_to=rescued_to,
        quarantined=quarantined,
    )
    return "clobber_recovered"


# ---------------------------------------------------------------------------
# Verben
# ---------------------------------------------------------------------------


def cmd_save(adapter: AgyAdapter, slot: str, label: str | None) -> str:
    validate_slot(slot)
    with switch_lock():
        try:
            token_bytes = (live_home() / adapter.token_rel).read_bytes()
        except OSError:
            token_bytes = None
        live_email = identity_from_bytes(token_bytes) if token_bytes is not None else None
        if live_email is None:
            raise CliError("Kein lesbares Live-Token zum Sichern gefunden.", 3)
        reg = load_registry()
        block = provider_block(reg, adapter)
        owner = slot_for_email(block, live_email)
        if owner is not None and owner["id"] != slot:
            raise CliError(f"Diese E-Mail gehört bereits zu Slot {owner['id']}.", 4)
        entry = find_slot(block, slot)
        if entry is not None and entry.get("email") != live_email:
            raise CliError(f"Slot {slot} gehört zu einer anderen E-Mail; bitte einen neuen Slot wählen.", 4)
        mirror_live_to_profile(adapter, profile_root(adapter, slot), token_bytes)
        if entry is None:
            entry = {"id": slot, "label": label or slot, "email": live_email, "state": ACTIVE}
            block["slots"].append(entry)
        else:
            entry["state"] = ACTIVE
            if label:
                entry["label"] = label
        mark_active(block, slot)
        save_registry(reg)
    return f"Slot {slot} ({live_email}) gesichert und als aktiv registriert."


OPENER_SHIM_TEMPLATE = r'''#!__PYTHON__
"""xdg-open-Ersatz für `llm-account login`: öffnet die OAuth-URL im laufenden Axi-Chrome.

Reihenfolge: (a) Zwischenablage, (b) CDP-Tab im laufenden Chrome, (c) Windows-Chrome-Debugprofil,
(d) Standardbrowser über PowerShell. Niemals /usr/bin/google-chrome.
"""
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

CLIP_DEFAULT = "/mnt/c/Windows/System32/clip.exe"
PS_DEFAULT = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
CHROME_EXE = r"C:\Program Files\Google\Chrome\Application\chrome.exe"
CMD_EXE = os.environ.get("LLM_ACCOUNTS_CMD_EXE") or "/mnt/c/Windows/System32/cmd.exe"
CDP_TIMEOUT_S = 3
CLIP_TIMEOUT_S = 2
PS_TIMEOUT_S = 15


def windows_local_app_data():
    """%LOCALAPPDATA% of the current Windows user, read through cmd.exe; None if unavailable."""
    try:
        done = subprocess.run(
            [CMD_EXE, "/C", "echo %LOCALAPPDATA%"],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            cwd="/mnt/c",
            timeout=CDP_TIMEOUT_S,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    value = done.stdout.strip()
    return value if re.match(r"^[A-Za-z]:\\", value) else None


def chrome_profile_dir():
    """Windows path of the dedicated Chrome debug profile; LLM_ACCOUNT_CHROME_PROFILE overrides it."""
    override = os.environ.get("LLM_ACCOUNT_CHROME_PROFILE")
    if override:
        return override
    local_app_data = windows_local_app_data()
    if not local_app_data:
        return None
    return local_app_data.rstrip("\\") + r"\Google\Chrome-Debug-Profile"


def info(message):
    print(message, file=sys.stderr)


def valid_url(url):
    if "\n" in url or "\r" in url:
        return False
    parts = urllib.parse.urlsplit(url)
    return parts.scheme in ("http", "https") and bool(parts.netloc)


def clip_copy(url):
    clip = os.environ.get("LLM_ACCOUNTS_CLIP", CLIP_DEFAULT)
    if not clip:
        return False
    if not os.path.exists(clip):
        clip = shutil.which("clip.exe")
    if not clip:
        return False
    try:
        subprocess.run([clip], input=url.encode("utf-8"), timeout=CLIP_TIMEOUT_S, check=True, capture_output=True)
    except (OSError, subprocess.SubprocessError):
        return False
    return True


def gateway_hosts():
    """Default-Gateway aus /proc/net/route (Little-Endian-Hex), unter WSL der Windows-Host."""
    try:
        with open("/proc/net/route", encoding="utf-8") as fh:
            rows = fh.readlines()[1:]
    except OSError:
        return []
    for row in rows:
        fields = row.split()
        if len(fields) >= 3 and fields[1] == "00000000":
            try:
                raw = bytes.fromhex(fields[2])[::-1]
            except ValueError:
                return []
            return [".".join(str(octet) for octet in raw)]
    return []


def cdp_hosts():
    explicit = os.environ.get("LLM_ACCOUNTS_CDP_HOST")
    if explicit:
        return [explicit]
    hosts = ["127.0.0.1"]
    for host in gateway_hosts():
        if host not in hosts:
            hosts.append(host)
    return hosts


def open_in_cdp(url):
    """Öffnet die URL als neuen Tab. PUT zuerst, GET für ältere Chrome-Versionen. Liefert den Host oder None."""
    port = os.environ.get("LLM_ACCOUNTS_CDP_PORT", "9222")
    target = "/json/new?" + urllib.parse.quote(url, safe="")
    for host in cdp_hosts():
        for method in ("PUT", "GET"):
            request = urllib.request.Request(
                f"http://{host}:{port}{target}", method=method, data=b"" if method == "PUT" else None
            )
            try:
                with urllib.request.urlopen(request, timeout=CDP_TIMEOUT_S) as response:
                    if response.status == 200:
                        return host
            except urllib.error.HTTPError:
                continue
            except (urllib.error.URLError, OSError):
                break
    return None


def powershell():
    found = shutil.which("powershell.exe")
    if found:
        return found
    return PS_DEFAULT if os.path.exists(PS_DEFAULT) else None


def wslenv_with_login_url(existing):
    """WSLENV mit LLM_LOGIN_URL, bestehende Einträge und deren Flags bleiben erhalten.

    Ohne Flag wird die Variable von WSL an den Windows-Prozess weitergereicht; ein Pfad-Flag wäre falsch.
    """
    entries = [entry for entry in (existing or "").split(":") if entry]
    if "LLM_LOGIN_URL" not in {entry.split("/", 1)[0] for entry in entries}:
        entries.append("LLM_LOGIN_URL")
    return ":".join(entries)


def run_powershell(ps, command, url):
    env = dict(os.environ, LLM_LOGIN_URL=url, WSLENV=wslenv_with_login_url(os.environ.get("WSLENV")))
    try:
        done = subprocess.run([ps, "-NoProfile", "-Command", command], env=env, capture_output=True,
                              timeout=PS_TIMEOUT_S, check=False, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return False
    return done.returncode == 0


def main(argv):
    if len(argv) < 2:
        info("[llm-account] Keine URL übergeben.")
        return 2
    url = argv[1]
    if not valid_url(url):
        info("[llm-account] Abgelehnt: nur http(s)-URLs werden geöffnet.")
        return 2
    copied = clip_copy(url)
    if open_in_cdp(url):
        if copied:
            info("[llm-account] Login-URL in Axi-Chrome geöffnet & in Zwischenablage kopiert.")
        else:
            info("[llm-account] Login-URL in Axi-Chrome geöffnet (Zwischenablage nicht verfügbar).")
        return 0
    ps = powershell()
    if ps is None:
        info(f"[llm-account] Kein Browser-Start möglich. Login-URL manuell öffnen: {url}")
        return 1
    profile = chrome_profile_dir()
    # Start-Process verkettet das Array ohne Quoting; der Profilpfad braucht deshalb eigene doppelte
    # Anführungszeichen in der nativen Befehlszeile. Einfache Anführungszeichen im PowerShell-Literal verdoppeln.
    profile_ps = (profile or "").replace("'", "''")
    chrome_cmd = (f"Start-Process '{CHROME_EXE}' -ArgumentList "
                  f"@('--user-data-dir=\"{profile_ps}\"', $env:LLM_LOGIN_URL)") if profile else ""
    if profile and run_powershell(ps, chrome_cmd, url):
        info("[llm-account] Axi-Chrome nicht erreichbar; Login-URL im Windows-Chrome-Debugprofil geöffnet.")
        return 0
    if run_powershell(ps, "Start-Process $env:LLM_LOGIN_URL", url):
        info("[llm-account] Login-URL über den Standardbrowser geöffnet.")
        return 0
    info(f"[llm-account] Browser-Start fehlgeschlagen. Login-URL manuell öffnen: {url}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
'''


def opener_shim_source() -> str:
    return OPENER_SHIM_TEMPLATE.replace("__PYTHON__", sys.executable)


def cmd_login(adapter: AgyAdapter, slot: str, label: str | None) -> str:
    validate_slot(slot)
    stage = ensure_owner_only_dir(state_dir() / "profiles" / adapter.name / f".login-{slot}-{os.getpid()}")
    try:
        atomic_write(stage / adapter.onboarding_rel, json.dumps(adapter.onboarding_seed).encode("utf-8"), owner_only=True)
        # xdg-open-Ersatz nur für diesen interaktiven Lauf; wird mit dem Staging-Verzeichnis gelöscht.
        bin_dir = stage / "bin"
        bin_dir.mkdir(exist_ok=True)
        os.chmod(bin_dir, 0o700)
        shim = bin_dir / "xdg-open"
        atomic_write(shim, opener_shim_source().encode("utf-8"), mode=0o700)
        env = dict(os.environ, HOME=str(stage))
        env["PATH"] = os.pathsep.join([str(bin_dir), os.environ.get("PATH", "")])
        env["BROWSER"] = str(shim)
        # Interaktiver OAuth-Flow: stdin/stdout bleiben am Terminal. Direktpfad, nie $PATH-Wrapper.
        proc = subprocess.run([agy_binary(), "-p", "/usage"], env=env, check=False)
        email = read_id_email(stage / adapter.token_rel)
        if email is None:
            raise CliError(f"Login für {slot} hat kein lesbares Token hinterlassen (agy Exit {proc.returncode}).", 5)
        with switch_lock():
            reg = load_registry()
            block = provider_block(reg, adapter)
            owner = slot_for_email(block, email)
            if owner is not None and owner["id"] != slot:
                raise CliError(f"Diese E-Mail gehört bereits zu Slot {owner['id']}.", 4)
            entry = find_slot(block, slot)
            if entry is not None and entry.get("email") != email:
                raise CliError(f"Slot {slot} gehört zu einer anderen E-Mail; Login verworfen.", 4)
            copy_present(stage, profile_root(adapter, slot), adapter.profile_rels, owner_only=True)
            if entry is None:
                block["slots"].append({"id": slot, "label": label or slot, "email": email, "state": ACTIVE})
            else:
                entry["state"] = ACTIVE
                if label:
                    entry["label"] = label
            save_registry(reg)
    finally:
        shutil.rmtree(stage, ignore_errors=True)
    return f"Slot {slot} ({email}) angemeldet und registriert. Das aktive Konto blieb unverändert."


def cmd_use(adapter: AgyAdapter, slot: str) -> str:
    validate_slot(slot)
    with switch_lock():
        reg = load_registry()
        target = find_slot(provider_block(reg, adapter), slot)
        if target is None:
            raise CliError(f"Unbekannter Slot {slot}.", 4)
        if target.get("state") != ACTIVE:
            raise CliError(f"Slot {slot} ist retired und kann nicht aktiviert werden.", 4)
        pdir = profile_root(adapter, slot)
        if not (pdir / adapter.token_rel).exists():
            raise CliError(f"Profil {slot} ist unvollständig; zuerst save oder login ausführen.", 3)
        heal_result = heal_locked(adapter)
        if heal_result == "unreadable":
            raise CliError("Live-Token ist nicht lesbar; Wechsel abgebrochen.", 3)
        reg = load_registry()
        block = provider_block(reg, adapter)
        previous = block.get("active_account")
        # Vor dem Kopieren prüfen: stimmen die Live-Dateien nach heal bereits mit dem Slot überein?
        unchanged = previous == slot and _live_matches_profile(adapter, pdir)
        # Ziel vollständig lesen, bevor die Live-Dateien angefasst werden; alte Live-Dateien für ein Zurücksetzen sichern.
        target_token = _read_or_none(pdir / adapter.token_rel)
        target_scope = _read_or_none(pdir / adapter.scope_rel)
        if target_token is None:
            raise CliError(f"Profil {slot} ist unvollständig; Token nicht lesbar. Wechsel abgebrochen.", 3)
        if (pdir / adapter.scope_rel).exists() and target_scope is None:
            raise CliError(f"Profil {slot} hat einen nicht lesbaren Scope. Wechsel abgebrochen.", 3)
        old_token = _read_or_none(live_home() / adapter.token_rel)
        old_scope = _read_or_none(live_home() / adapter.scope_rel)
        try:
            install_profile_to_live(adapter, pdir)
            block["active_account"] = slot
            if not unchanged:
                block["switch_epoch"] = time.time()  # unveränderte Panes dürfen nicht als veraltet gelten
            save_registry(reg)
        except BaseException:
            _restore_live(adapter, old_token, old_scope)
            raise
        removed = invalidate_caches()
        log_event(
            "switch",
            provider=adapter.name,
            from_slot=previous,
            to_slot=slot,
            heal=heal_result,
            caches_removed=len(removed),
        )
    return f"Aktives Konto gewechselt: {previous or '-'} -> {slot}."


def _read_or_none(path: Path) -> bytes | None:
    try:
        return path.read_bytes()
    except OSError:
        return None


def _restore_live(adapter: AgyAdapter, old_token: bytes | None, old_scope: bytes | None) -> None:
    """Stellt die vorherigen Live-Dateien wieder her, wenn ein Kontowechsel auf halber Strecke scheitert.

    Ohne owner_only: das Live-Home liegt nicht unter dem State-Verzeichnis, owner_only würde dort abbrechen.
    Fehlte vorher eine Datei, wird die neu geschriebene wieder entfernt.
    """
    if old_token is not None:
        atomic_write(live_home() / adapter.token_rel, old_token)
    else:
        _unlink_if_present(live_home() / adapter.token_rel)
    if old_scope is not None:
        atomic_write(live_home() / adapter.scope_rel, old_scope)
    else:
        _unlink_if_present(live_home() / adapter.scope_rel)


def _live_matches_profile(adapter: AgyAdapter, pdir: Path) -> bool:
    def same(rel: Path) -> bool:
        try:
            return (live_home() / rel).read_bytes() == (pdir / rel).read_bytes()
        except OSError:
            return False

    if not same(adapter.token_rel):
        return False
    if (pdir / adapter.scope_rel).exists():
        return same(adapter.scope_rel)
    return not (live_home() / adapter.scope_rel).exists()


def cmd_retire(adapter: AgyAdapter, slot: str, force: bool) -> str:
    validate_slot(slot)
    with switch_lock():
        reg = load_registry()
        block = provider_block(reg, adapter)
        target = find_slot(block, slot)
        if target is None:
            raise CliError(f"Unbekannter Slot {slot}.", 4)
        if target.get("state") == RETIRED:
            return f"Slot {slot} ist bereits retired."
        active_slots = [s for s in block["slots"] if s.get("state") == ACTIVE]
        if target.get("state") == ACTIVE and len(active_slots) == 1 and not force:
            raise CliError(f"Slot {slot} ist der letzte aktive Slot; --force erforderlich.", 4)
        if block.get("active_account") == slot and not force:
            raise CliError(f"Slot {slot} ist das aktive Konto; --force erforderlich.", 4)
        target["state"] = RETIRED
        save_registry(reg)
        (state_dir() / "quotas.json").unlink(missing_ok=True)
        log_event("retire", provider=adapter.name, slot=slot, forced=force)
    return f"Slot {slot} ist retired."


def cmd_heal(adapter: AgyAdapter) -> str:
    with switch_lock():
        return heal_locked(adapter)


# ---------------------------------------------------------------------------
# Quota-Status
# ---------------------------------------------------------------------------


def parse_usage_tsv(text: str, adapter: AgyAdapter = ADAPTERS["agy"]) -> dict[str, dict[str, Any]]:
    """Zeilen: ModelGroup \\t LimitType \\t Remaining% \\t ResetIso."""
    lanes: dict[str, dict[str, Any]] = {}
    for line in text.splitlines():
        parts = line.split("\t")
        if len(parts) < 4:
            continue
        group, limit, pct_raw, reset_raw = (p.strip() for p in parts[:4])
        lane = next((ln for marker, ln in adapter.lane_markers if marker in group), None)
        if lane is None:
            continue
        match = re.fullmatch(r"(\d{1,3})%?", pct_raw)
        if not match or int(match.group(1)) > 100:
            continue
        pct = int(match.group(1))
        if "Five Hour" not in limit and "Weekly" not in limit:
            continue  # unbekannter Limit-Typ: keine leere, scheinbar gesunde Lane
        entry = lanes.setdefault(lane, {"five_hour_pct": None, "five_hour_reset": None, "weekly_pct": None, "weekly_reset": None})
        if "Five Hour" in limit:
            entry["five_hour_pct"], entry["five_hour_reset"] = pct, reset_raw or None
        elif "Weekly" in limit:
            entry["weekly_pct"], entry["weekly_reset"] = pct, reset_raw or None
    return lanes


def probe_slot(adapter: AgyAdapter, slot_id: str) -> dict[str, Any]:
    root = profile_root(adapter, slot_id)
    token = root / adapter.token_rel
    if read_id_email(token) is None:
        return {"auth_state": AUTH_RELOGIN, "lanes": {}}
    env = dict(os.environ, HOME=str(root), BROWSER="false", DISPLAY="", XDG_OPEN="")
    try:
        proc = subprocess.run(
            adapter.usage_argv(),
            env=env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=USAGE_TIMEOUT_S,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return {"auth_state": AUTH_UNAVAILABLE, "lanes": {}}
    lanes = parse_usage_tsv(proc.stdout, adapter)
    if lanes and proc.returncode == 0:
        return {"auth_state": AUTH_OK, "lanes": lanes}
    haystack = f"{proc.stdout}\n{proc.stderr}".lower()
    if any(marker in haystack for marker in adapter.auth_markers):
        return {"auth_state": AUTH_RELOGIN, "lanes": {}}
    return {"auth_state": AUTH_UNAVAILABLE, "lanes": {}}


def probe_slots(adapter: AgyAdapter, slots: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    if not slots:
        return {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(slots)) as pool:
        futures = {s["id"]: pool.submit(probe_slot, adapter, s["id"]) for s in slots}
        return {slot_id: fut.result() for slot_id, fut in futures.items()}


def _recommend_slot(accounts: list[dict[str, Any]], lane: str) -> str | None:
    healthy = [a for a in accounts if a["auth_state"] == AUTH_OK and a["lanes"][lane]["weekly_pct"] is not None]
    strict = [
        a
        for a in healthy
        if a["lanes"][lane]["weekly_pct"] >= FLOOR_PCT
        and (a["lanes"][lane]["five_hour_pct"] or 0) >= FEFO_FIVE_HOUR_MIN_PCT
    ]
    if strict:
        best = min(
            strict,
            key=lambda a: iso_to_epoch(a["lanes"][lane]["weekly_reset"]) or float("inf"),
        )
        return best["id"]
    if healthy:
        best = max(healthy, key=lambda a: a["lanes"][lane]["five_hour_pct"] or -1)
        return best["id"]
    return None


def _effective_lane(accounts: list[dict[str, Any]], lane: str, active_id: str | None, now: float) -> dict[str, Any]:
    pooled = [a for a in accounts if a["auth_state"] == AUTH_OK and a["lanes"][lane]["weekly_pct"] is not None]
    spendable = [
        a for a in pooled
        if (a["lanes"][lane]["weekly_pct"] or 0) > 0 and (a["lanes"][lane]["five_hour_pct"] or 0) > 0
    ]
    refill_candidates = [
        iso_to_epoch(a["lanes"][lane]["weekly_reset"])
        for a in pooled
        if a["id"] != active_id
    ] if len(pooled) >= 2 else []
    refill_candidates = [c for c in refill_candidates if c is not None]
    next_refill = min(refill_candidates) if refill_candidates else None
    bridge_days = max((next_refill - now) / DAY_S, 0.25) if next_refill is not None else 7.0
    bridge_sum = sum(max(0, a["lanes"][lane]["weekly_pct"] - FLOOR_PCT) for a in pooled)
    bridge = round(bridge_sum / bridge_days, 1) if pooled else 0.0
    steady = round(len(pooled) * (100 - FLOOR_PCT) / 7.0, 1)
    return {
        "pooled_count": len(pooled),
        "spendable_count": len(spendable),
        "effective_weekly_pct": max(a["lanes"][lane]["weekly_pct"] for a in pooled) if pooled else None,
        "next_refill_at": utc_iso(next_refill) if next_refill is not None else None,
        "bridge_daily_pct": bridge,
        "steady_daily_pct": steady,
        "sustainable_daily_pct": round(min(bridge, steady), 1),
        "recommended_slot_fefo": _recommend_slot(accounts, lane),
    }


def build_snapshot(
    adapter: AgyAdapter,
    block: dict[str, Any],
    probes: dict[str, dict[str, Any]],
    now: float,
    heal_result: str,
) -> dict[str, Any]:
    active_id = block.get("active_account")
    accounts: list[dict[str, Any]] = []
    for slot in block["slots"]:
        if slot.get("state") != ACTIVE:
            continue
        probe = probes.get(slot["id"], {"auth_state": AUTH_UNAVAILABLE, "lanes": {}})
        lanes = {
            lane: {"five_hour_pct": None, "five_hour_reset": None, "weekly_pct": None, "weekly_reset": None}
            for lane in adapter.lanes
        }
        for lane, data in probe.get("lanes", {}).items():
            if lane in lanes:
                lanes[lane].update(data)
        accounts.append({
            "id": slot["id"],
            "label": slot.get("label"),
            "email": slot.get("email"),
            "state": ACTIVE,
            "is_active": slot["id"] == active_id,
            "auth_state": probe["auth_state"],
            "lanes": lanes,
        })
    provider_out = {
        "active_account": active_id,
        "switch_epoch": block.get("switch_epoch"),
        "primary_lane": adapter.primary_lane,
        "accounts": accounts,
        "effective": {
            "lanes": {lane: _effective_lane(accounts, lane, active_id, now) for lane in adapter.lanes}
        },
    }
    return {"version": 1, "updated_epoch": int(now), "heal": heal_result, "providers": {adapter.name: provider_out}}


def fetch_provider_status(adapter: AgyAdapter, *, write_quotas: bool = False, now: float | None = None) -> dict[str, Any]:
    current = time.time() if now is None else now
    with switch_lock():
        heal_result = heal_locked(adapter)
        block = copy.deepcopy(provider_block(load_registry(), adapter))
    # Probes laufen bewusst ohne Lock: agy -p /usage kann bis 12 s dauern.
    active_slots = [s for s in block["slots"] if s.get("state") == ACTIVE]
    probes = probe_slots(adapter, active_slots)
    snapshot = build_snapshot(adapter, block, probes, current, heal_result)
    if write_quotas:
        # Ein parallel laufendes retire oder use hat die Auswertung während der Probes überholt. Dann wird
        # nichts geschrieben: ein überholter Snapshot könnte einen ausgemusterten Slot wieder als gesund melden.
        with switch_lock():
            if copy.deepcopy(provider_block(load_registry(), adapter)) != block:
                snapshot["registry_changed"] = True
                return snapshot
            atomic_write(
                state_dir() / "quotas.json",
                (json.dumps(snapshot, indent=2, ensure_ascii=False) + "\n").encode("utf-8"),
                owner_only=True,
            )
    return snapshot


def cmd_status(adapter: AgyAdapter, as_json: bool, write_quotas: bool) -> str:
    snapshot = fetch_provider_status(adapter, write_quotas=write_quotas)
    if as_json:
        return json.dumps(snapshot, indent=2, ensure_ascii=False)
    prov = snapshot["providers"][adapter.name]
    lines = [f"Aktiv: {prov['active_account'] or '-'} (Heal: {snapshot['heal']})"]
    for acc in prov["accounts"]:
        gem = acc["lanes"][adapter.primary_lane]
        lines.append(
            f"  {acc['id']} ({acc['label']}): auth={acc['auth_state']} "
            f"Woche={gem['weekly_pct']}% 5h={gem['five_hour_pct']}%"
        )
    eff = prov["effective"]["lanes"][adapter.primary_lane]
    lines.append(f"Empfehlung (FEFO): {eff['recommended_slot_fefo'] or '-'}")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# stale-panes: agy-Prozesse, die vor dem letzten Kontowechsel gestartet wurden
# ---------------------------------------------------------------------------


def _read_btime(root: Path) -> int:
    try:
        for line in (root / "stat").read_text(encoding="utf-8").splitlines():
            if line.startswith("btime "):
                return int(line.split()[1])
    except (OSError, ValueError) as exc:
        raise CliError(f"/proc/stat nicht lesbar: {exc}", 3) from exc
    raise CliError("btime fehlt in /proc/stat.", 3)


def _herdr_env(entry: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    try:
        raw = (entry / "environ").read_bytes()
    except OSError:
        return result
    for item in raw.split(b"\0"):
        key, sep, value = item.partition(b"=")
        if sep and key in (b"HERDR_PANE_ID", b"HERDR_TAB_ID"):
            result[key.decode()] = value.decode(errors="replace")
    return result


def _cmd_option(argv: list[str], name: str) -> str | None:
    """Wert einer Option in `--name wert` oder `--name=wert`."""
    for idx, arg in enumerate(argv):
        if arg == name:
            return argv[idx + 1] if idx + 1 < len(argv) else None
        if arg.startswith(name + "="):
            return arg.split("=", 1)[1]
    return None


def scan_agy_processes(root: Path, clk_tck: int | None = None) -> list[dict[str, Any]]:
    """Alle agy-Prozesse aus /proc mit Startzeit, Pane, Konversation, Modell und cwd."""
    btime = _read_btime(root)
    ticks = clk_tck or os.sysconf("SC_CLK_TCK")
    records: list[dict[str, Any]] = []
    for entry in sorted(root.iterdir(), key=lambda p: p.name):
        if not entry.name.isdigit():
            continue
        try:
            argv = [a.decode(errors="replace") for a in (entry / "cmdline").read_bytes().split(b"\0") if a]
            raw_stat = (entry / "stat").read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if not argv or os.path.basename(argv[0]) != "agy":
            continue
        # comm kann Leerzeichen und Klammern enthalten: am letzten ')' splitten.
        fields = raw_stat[raw_stat.rfind(")") + 2:].split()
        try:
            # Bruchteile der Sekunde bleiben erhalten: der Vergleich mit switch_epoch braucht die Reihenfolge.
            start_epoch = btime + int(fields[19]) / ticks
        except (IndexError, ValueError):
            continue
        conversation = _cmd_option(argv, "--conversation")
        try:
            cwd = os.readlink(entry / "cwd")
        except OSError:
            cwd = None
        env = _herdr_env(entry)
        records.append({
            "pid": int(entry.name),
            "start_epoch": start_epoch,
            "conversation_id": conversation,
            "model": _cmd_option(argv, "--model") or _cmd_option(argv, "-m"),
            "cwd": cwd,
            "pane_id": env.get("HERDR_PANE_ID"),
            "tab_id": env.get("HERDR_TAB_ID"),
            "cmdline_kind": "conversation" if conversation else "interactive",
        })
    return records


def find_stale_panes(root: Path, switch_epoch: int | None, clk_tck: int | None = None) -> list[dict[str, Any]]:
    if switch_epoch is None:
        return []
    found: list[dict[str, Any]] = []
    for rec in scan_agy_processes(root, clk_tck):
        if rec["start_epoch"] >= switch_epoch:
            continue
        found.append({
            "pid": rec["pid"],
            "start_epoch": rec["start_epoch"],
            "switch_epoch": switch_epoch,
            "conversation_id": rec["conversation_id"],
            "pane_id": rec["pane_id"],
            "tab_id": rec["tab_id"],
            "cmdline_kind": rec["cmdline_kind"],
        })
    return found


# ---------------------------------------------------------------------------
# bounce-stale: In-Place-Warm-Bounce einer Pane mit veraltetem agy-Prozess
# ---------------------------------------------------------------------------

BOUNCE_MAX_BYTES = 150 * 1024 * 1024
BOUNCE_EXIT_TIMEOUT_S = 15.0
BOUNCE_START_TIMEOUT_S = 60.0
BOUNCE_SHELL_TIMEOUT_S = 10.0
HERDR_BIN_DEFAULT = str(Path.home() / ".local" / "bin" / "herdr")
CONVERSATION_UUID_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
MODEL_RE = re.compile(r"[a-zA-Z0-9._:/-]{1,80}")
SHELL_NAMES = frozenset({"zsh", "bash", "sh"})
READY_PANE_STATUSES = frozenset({"idle", "done"})
HERDR_CHAT_DEFAULT = str(
    Path.home() / ".codex" / "skills" / "herdr-project-orchestration" / "scripts" / "herdr_chat.py"
)
WAKEUP_BETREFF = "Konto-Wechsel abgeschlossen: Arbeit fortsetzen"
WAKEUP_TEXT = ("Konto-Wechsel abgeschlossen — bitte unterbrochene Arbeit fortsetzen "
               "und ungelesene Briefkasten-Nachrichten abarbeiten.")
WAKEUP_BANNER = "esc to dismiss"
WAKEUP_READY_TIMEOUT_S = 20.0
WAKEUP_WORK_TIMEOUT_S = 10.0
PROTECTED_TAB_PREFIXES = ("vermarkter", "design studio", "architekt", "senior-architekt", "eskalations-architekt")
# AUTH-HEAL-1a: menschliche Diskussions-Tabs sind immer gesperrt, Architekten-Tabs nur beim 401-Grund.
HUMAN_PROTECTED_TAB_PREFIXES = ("vermarkter", "design studio")
ARCHITEKT_TAB_PREFIXES = ("architekt", "senior-architekt", "eskalations-architekt")
BOUNCE_REASONS = ("switch_epoch", "auth_401", "manual")
QUOTA_SNAPSHOT_FILE = "quotas.json"
QUOTA_MAX_AGE_S = 300.0
ANTI_LOOP_COOLDOWN_S = 1800.0
ANTI_LOOP_STATE_FILE = "bounce-state.json"
ANTI_LOOP_LOCK_FILE = "bounce-state.lock"
QUIET_RECHECK_S = 30.0
HARNESS_INFRA_NAMES = frozenset({"mcp", "language_server", "statusline.sh"})
ACTIVE_STATUS_RE = re.compile(r"esc to interrupt|\b[1-9]\d* task\(s\)|\brunning\b", re.IGNORECASE)
AUTH_401_RE = re.compile(
    r"\b(?:HTTP[/\s]*401|status(?:\s+code)?[:=\s]+401|error[:\s]+401|401\s+Unauthorized)\b"
    r"|UNAUTHENTICATED|invalid_grant|Unauthorized",
    re.IGNORECASE,
)
REPLAY_ID_RE = re.compile(r"[A-Za-z0-9_.:-]{1,80}")
BRIEFKASTEN_DEFAULT = "~/.local/state/herdr/briefkasten"


class HerdrCli:
    """Dünne Hülle um die herdr-CLI. Tests ersetzen sie durch einen Fake."""

    def __init__(self, binary: str | None = None) -> None:
        self.binary = binary or os.environ.get("HERDR_BIN_PATH") or HERDR_BIN_DEFAULT

    def _run(self, *args: str) -> str:
        try:
            proc = subprocess.run([self.binary, *args], capture_output=True, text=True, timeout=20, check=False)
        except (subprocess.TimeoutExpired, OSError) as exc:
            raise CliError(f"herdr {' '.join(args[:2])} nicht ausführbar: {exc}", 5) from exc
        if proc.returncode != 0:
            raise CliError(f"herdr {' '.join(args[:2])} schlug fehl: {proc.stderr.strip()[:200]}", 5)
        return proc.stdout

    def pane_list(self) -> list[dict[str, Any]]:
        try:
            return json.loads(self._run("pane", "list"))["result"]["panes"]
        except (ValueError, KeyError, TypeError) as exc:
            raise CliError(f"herdr pane list lieferte unerwartetes JSON: {exc}", 5) from exc

    def tab_label(self, tab_id: str) -> str | None:
        try:
            return json.loads(self._run("tab", "get", tab_id))["result"]["tab"]["label"]
        except (ValueError, KeyError, TypeError):
            return None

    def screen_text(self, pane_id: str) -> str:
        return self._run("pane", "read", pane_id, "--source", "visible")

    def foreground_names(self, pane_id: str) -> list[str]:
        try:
            data = json.loads(self._run("pane", "process-info", "--pane", pane_id))
            return [p.get("name", "") for p in data["result"]["process_info"]["foreground_processes"]]
        except (ValueError, KeyError, TypeError) as exc:
            raise CliError(f"herdr pane process-info lieferte unerwartetes JSON: {exc}", 5) from exc

    def run(self, pane_id: str, text: str) -> None:
        self._run("pane", "run", pane_id, text)

    def send_keys(self, pane_id: str, key: str) -> None:
        self._run("pane", "send-keys", pane_id, key)


def protected_tab(label: str | None) -> bool:
    if not label:
        return False
    normalized = re.sub(r"^\W+", "", label).strip().lower()
    return any(normalized.startswith(prefix) for prefix in PROTECTED_TAB_PREFIXES)


def _tab_name(label: str | None) -> str:
    return re.sub(r"^\W+", "", label or "").strip().lower()


def human_protected_tab(label: str | None) -> bool:
    return any(_tab_name(label).startswith(p) for p in HUMAN_PROTECTED_TAB_PREFIXES)


def architekt_tab(label: str | None) -> bool:
    return any(_tab_name(label).startswith(p) for p in ARCHITEKT_TAB_PREFIXES)


def focus_blockers(pane: dict[str, Any], allow_focused: bool) -> list[str]:
    """Bedingung 3: nur `focused == false` ist frei. Ein fehlendes Feld gilt als unbekannt (fail-closed)."""
    focused = pane.get("focused")
    if focused is True:
        return [] if allow_focused else ["pane_focused"]
    if focused is False:
        return []
    return ["focus_unbekannt"]


def _read_status(entry: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    try:
        text = (entry / "status").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return result
    for line in text.splitlines():
        key, sep, value = line.partition(":")
        if sep:
            result[key.strip()] = value.strip()
    return result


def _children_file_pids(root: Path, pid: int) -> set[int]:
    kids: set[int] = set()
    try:
        task_dirs = list((root / str(pid) / "task").iterdir())
    except OSError:
        return kids
    for tid in task_dirs:
        try:
            raw = (tid / "children").read_text(encoding="utf-8").split()
        except OSError:
            continue
        kids.update(int(x) for x in raw if x.isdigit())
    return kids


def _is_harness_infra(name: str, argv: list[str]) -> bool:
    candidates = {name} | {os.path.basename(a) for a in argv[:2]}
    return any(
        c in HARNESS_INFRA_NAMES or c.startswith("mcp-") or c.startswith("language_server")
        for c in candidates
    )


def _describe_descendant(root: Path, pid: int, btime: int, ticks: int, now: float) -> dict[str, Any] | None:
    entry = root / str(pid)
    status = _read_status(entry)
    if status.get("State", "").startswith("Z"):
        return None  # Zombie: beendet, nur noch Eintrag in der Prozesstabelle
    try:
        argv = [a.decode(errors="replace") for a in (entry / "cmdline").read_bytes().split(b"\0") if a]
    except OSError:
        argv = []
    name = status.get("Name", "")
    if _is_harness_infra(name, argv):
        return None
    cpu_s = age_s = 0.0
    try:
        raw = (entry / "stat").read_text(encoding="utf-8", errors="replace")
        fields = raw[raw.rfind(")") + 2:].split()
        cpu_s = (int(fields[11]) + int(fields[12])) / ticks
        age_s = max(0.0, now - (btime + int(fields[19]) / ticks))
    except (OSError, IndexError, ValueError):
        pass
    return {"pid": pid, "age_s": round(age_s, 1), "cpu_s": round(cpu_s, 2), "cmd": (" ".join(argv) or name)[:120]}


def find_active_descendants(root: Path, agent_pid: int, clk_tck: int | None = None) -> list[dict[str, Any]]:
    """Bedingung 4b: transitiver Nachfahrenbaum ab agent_pid ohne feste Harness-Infrastruktur. Nie killen."""
    ppid_map: dict[int, set[int]] = {}
    for entry in root.iterdir():
        if not entry.name.isdigit():
            continue
        ppid = _read_status(entry).get("PPid", "")
        if ppid.isdigit():
            ppid_map.setdefault(int(ppid), set()).add(int(entry.name))
    btime = _read_btime(root)
    ticks = clk_tck or os.sysconf("SC_CLK_TCK")
    now = time.time()
    found: list[dict[str, Any]] = []
    seen = {agent_pid}
    queue = [agent_pid]
    while queue:
        parent = queue.pop()
        for kid in sorted(ppid_map.get(parent, set()) | _children_file_pids(root, parent)):
            if kid in seen:
                continue
            seen.add(kid)
            queue.append(kid)
            rec = _describe_descendant(root, kid, btime, ticks, now)
            if rec is not None:
                found.append(rec)
    return found


def statusline_active(screen: str) -> bool:
    """Bedingung 4a: Statuszeile am Fuß der Pane zeigt laufende Arbeit."""
    tail = [line for line in screen.splitlines() if line.strip()][-3:]
    return any(ACTIVE_STATUS_RE.search(line) for line in tail)


def _load_anti_loop_state() -> dict[str, Any]:
    """Liest den Cooldown-Zustand ohne Seiteneffekte; ein fehlendes State-Verzeichnis ist ein leerer Zustand."""
    path = state_dir() / ANTI_LOOP_STATE_FILE
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return {}
    except OSError as exc:
        raise CliError(f"{ANTI_LOOP_STATE_FILE} nicht lesbar: {exc}", 3) from exc
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise CliError(f"{ANTI_LOOP_STATE_FILE} nicht lesbar: {exc}", 3) from exc
    if not isinstance(data, dict):
        raise CliError(f"{ANTI_LOOP_STATE_FILE} hat unerwartetes Format.", 3)
    return data


def _cooldown_in(data: dict[str, Any], pane_id: str, switch_epoch: float | None, now: float) -> float:
    entry = data.get(pane_id)
    if not isinstance(entry, dict):
        return 0.0
    try:
        last_ts = float(entry["ts"])
    except (KeyError, TypeError, ValueError) as exc:
        raise CliError(f"{ANTI_LOOP_STATE_FILE}: Eintrag für {pane_id} unvollständig.", 3) from exc
    last_epoch = entry.get("switch_epoch")
    if switch_epoch is not None and last_epoch is not None and switch_epoch > last_epoch:
        return 0.0
    return max(0.0, ANTI_LOOP_COOLDOWN_S - (now - last_ts))


def anti_loop_cooldown(pane_id: str, switch_epoch: float | None, now: float | None = None) -> float:
    """Bedingung 6: Restzeit in Sekunden. 0 heißt erlaubt. Ohne bekannten switch_epoch-Stand gilt die Sperre."""
    current = time.time() if now is None else now
    return _cooldown_in(_load_anti_loop_state(), pane_id, switch_epoch, current)


def record_bounce_attempt(pane_id: str, switch_epoch: float | None, now: float | None = None) -> None:
    """Prüft den Cooldown und bucht den Versuch in einem Schritt unter demselben Lock, bevor /exit läuft.

    Zwei parallele Aufrufe können so nicht beide die Prüfung bestehen: der zweite sieht die Buchung des ersten.
    Ein Versuch innerhalb des Cooldowns wird abgelehnt und überschreibt den bestehenden Eintrag nicht.
    """
    root = ensure_state()
    current = time.time() if now is None else now
    fd = os.open(root / ANTI_LOOP_LOCK_FILE, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        data = _load_anti_loop_state()
        remaining = _cooldown_in(data, pane_id, switch_epoch, current)
        if remaining > 0:
            raise CliError(f"Anti-Loop-Cap für {pane_id}: noch {round(remaining)} s Sperre, kein /exit.", 4)
        data[pane_id] = {"ts": current, "switch_epoch": switch_epoch}
        atomic_write(root / ANTI_LOOP_STATE_FILE,
                     (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8"), owner_only=True)
    finally:
        os.close(fd)


HEAL_PREVIEWS_HEALTHY = frozenset({"would_sync_or_current", "would_recover_clobber"})


def token_expiry_blocker(adapter: AgyAdapter, now: float) -> str | None:
    """Optionales Ablaufdatum in der Token-JSON (`expires_at` oder `expiry`, Epoch oder ISO-UTC). Nur lesend."""
    try:
        data = json.loads((live_home() / adapter.token_rel).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    raw = data.get("expires_at", data.get("expiry"))
    if raw is None:
        return None
    if isinstance(raw, (int, float)) and not isinstance(raw, bool):
        expiry = float(raw)
    else:
        expiry = iso_to_epoch(raw)
    if expiry is None:
        return "disk_token_unhealthy:ablauf_unlesbar"
    return "disk_token_unhealthy:token_abgelaufen" if expiry <= now else None


def disk_token_blockers(adapter: AgyAdapter, now: float | None = None, reason: str = "switch_epoch") -> list[str]:
    """Bedingung 1: rein lesender Token-Vortest. Kein Refresh, keine Schreibzugriffe.

    Immer blockiert: unlesbares Live-Token, abgelaufenes oder unlesbares Ablaufdatum, quotas.json mit
    veraltetem Stand (> 300 s), unlesbarem Inhalt, fehlendem aktiven Slot oder auth_state ungleich ok.
    Bei reason auth_401 zusätzlich fail-closed: jeder heal_preview-Wert außer den gesunden Fällen
    (also no_live_token, no_active, unknown_active) und eine fehlende quotas.json.
    Bei switch_epoch bleibt ein fehlendes Live-Token kein Blocker, weil heal es als gültiges Ergebnis führt.
    """
    blockers: list[str] = []
    current = time.time() if now is None else now
    preview = heal_preview(adapter)
    strict = reason == "auth_401"
    if preview == "unreadable":
        blockers.append("disk_token_unhealthy:unreadable")
    elif strict and preview not in HEAL_PREVIEWS_HEALTHY:
        blockers.append(f"disk_token_unhealthy:{preview}")
    if preview != "unreadable":
        expiry_blocker = token_expiry_blocker(adapter, current)
        if expiry_blocker:
            blockers.append(expiry_blocker)
    quota = state_dir() / QUOTA_SNAPSHOT_FILE
    if not quota.exists():
        if strict:
            blockers.append("disk_token_unhealthy:quotas_fehlt")
        return blockers
    try:
        snap = json.loads(quota.read_text(encoding="utf-8"))
        updated = float(snap["updated_epoch"])
        provider = snap["providers"][adapter.name]
    except (OSError, ValueError, KeyError, TypeError):
        return blockers + ["disk_token_unhealthy:quotas_unlesbar"]
    if current - updated > QUOTA_MAX_AGE_S:
        blockers.append("disk_token_unhealthy:quotas_veraltet")
    # Der Nachweis gilt nur für das Konto, das die Registry jetzt als aktiv führt. Ein Snapshot für ein anderes
    # Konto (oder eine ältere Wechselepoche) bestätigt den aktuellen Live-Stand nicht.
    reg_block = provider_block(load_registry(), adapter)
    if provider.get("active_account") != reg_block.get("active_account") or \
            provider.get("switch_epoch") != reg_block.get("switch_epoch"):
        blockers.append("disk_token_unhealthy:quotas_anderes_konto")
        return blockers
    active = provider.get("active_account")
    account = next((a for a in provider.get("accounts", []) if a.get("id") == active), None)
    if account is None:
        blockers.append("disk_token_unhealthy:aktiver_slot_fehlt")
    elif account.get("auth_state") != AUTH_OK:
        blockers.append(f"disk_token_unhealthy:auth_{account.get('auth_state')}")
    return blockers


def conversation_collision(root: Path, conversation: str | None, pid: int) -> bool:
    """Bedingung 5: eine zweite lebende agy-PID trägt dieselbe Konversation."""
    if not conversation:
        return False
    return any(r["conversation_id"] == conversation and r["pid"] != pid for r in scan_agy_processes(root))


def auth_401_evidence(screen: str) -> bool:
    tail = [line for line in screen.splitlines() if line.strip()][-15:]
    return any(AUTH_401_RE.search(line) for line in tail)


def briefkasten_unread(pane_id: str) -> int | None:
    """Anzahl ungelesener Nachrichten (oberste Ebene des Pane-Briefkastens, ohne gelesen/)."""
    base = Path(os.environ.get("HERDR_BRIEFKASTEN_DIR") or BRIEFKASTEN_DEFAULT).expanduser()
    folder = base / pane_id.replace(":", "_")
    try:
        return sum(1 for f in folder.glob("*.md") if f.is_file())
    except OSError:
        return None


def guard_blockers(
    adapter: AgyAdapter,
    pane: dict[str, Any],
    *,
    root: Path,
    pid: int,
    conversation: str | None,
    label: str | None,
    screen: str,
    switch_epoch: float | None,
    reason: str,
    allow_focused: bool,
    source: str | None,
) -> tuple[list[str], dict[str, Any]]:
    """Gemeinsame Sieben-Bedingungen-Prüfung für Dry-Run, --yes und die Frischprüfung vor /exit."""
    blockers: list[str] = []
    # None heißt: Tab-Beschriftung nicht lesbar. Das ist kein Freibrief, sondern unbekannter Schutz (fail-closed).
    if label is None:
        blockers.append("tab_schutz_unbekannt")
    if human_protected_tab(label):
        blockers.append("protected_human_tab")
    elif architekt_tab(label) and reason != "auth_401":
        blockers.append("protected_architekt_tab")
    if reason == "auth_401" and not source:
        blockers.append("zielbindung_quelle_fehlt")
    if reason == "auth_401" and not auth_401_evidence(screen):
        blockers.append("auth_401_nicht_belegt")
    blockers += focus_blockers(pane, allow_focused)
    descendants = find_active_descendants(root, pid)
    if descendants:
        blockers.append("active_or_hung_descendant")
    if statusline_active(screen):
        blockers.append("active_statusline")
    if conversation_collision(root, conversation, pid):
        blockers.append("conversation_uuid_collision")
    cooldown = anti_loop_cooldown(pane.get("pane_id", ""), switch_epoch)
    if cooldown > 0:
        blockers.append("anti_loop_cap_active")
    blockers += disk_token_blockers(adapter, reason=reason)
    return blockers, {"active_descendants": descendants, "cooldown_remaining_s": round(cooldown, 1)}


def live_blockers(status: str | None, draft: bool | None) -> list[str]:
    """Gemeinsame Live-Vorbedingungen für --dry-run und --yes: dieselbe Prüfung an beiden Stellen."""
    blockers: list[str] = []
    if status not in READY_PANE_STATUSES:
        blockers.append(f"Pane nicht bereit (agent_status {status!r}; erlaubt: {', '.join(sorted(READY_PANE_STATUSES))})")
    if draft is not False:
        blockers.append("Composer enthält Entwurf oder ist nicht eindeutig erkennbar")
    return blockers


def composer_draft(screen: str) -> bool | None:
    """True: Entwurfstext im Composer. False: Composer leer. None: Composer nicht eindeutig erkennbar."""
    lines = screen.splitlines()
    found: str | None = None
    for i in range(len(lines) - 2):
        if lines[i].strip().startswith("─") and lines[i + 2].strip().startswith("─"):
            prompt = lines[i + 1].lstrip()
            if prompt.startswith(">"):
                found = prompt[1:].strip()
    if found is None:
        return None
    return found != ""


def conversation_sizes(conversation_id: str) -> tuple[int, int]:
    """(db_bytes, brain_bytes). Die Schwelle gilt nur für db_bytes; brain_bytes ist informativ."""
    base = live_home() / ".gemini/antigravity-cli"
    db_bytes = sum(f.stat().st_size for f in (base / "conversations").glob(f"{conversation_id}.*") if f.is_file())
    brain = base / "brain" / conversation_id
    brain_bytes = sum(f.stat().st_size for f in brain.rglob("*") if f.is_file()) if brain.is_dir() else 0
    return db_bytes, brain_bytes


def heal_preview(adapter: AgyAdapter) -> str:
    """Read-only Vorschau dessen, was heal täte. Schreibt nichts."""
    live_token = live_home() / adapter.token_rel
    if not live_token.exists():
        return "no_live_token"
    live_email = read_id_email(live_token)
    if live_email is None:
        return "unreadable"
    block = provider_block(load_registry(), adapter)
    active_id = block.get("active_account")
    if not active_id:
        return "no_active"
    active = find_slot(block, active_id)
    if active is None:
        return "unknown_active"
    return "would_sync_or_current" if live_email == active.get("email") else "would_recover_clobber"


def plan_bounce(
    adapter: AgyAdapter,
    pane_id: str,
    herdr: Any,
    *,
    conversation: str | None = None,
    cwd: str | None = None,
    model: str | None = None,
    force_pane: bool = False,
    proc: Path | None = None,
    reason: str = "switch_epoch",
    allow_focused: bool = False,
    source: str | None = None,
    replay_msg_ids: list[str] | None = None,
) -> dict[str, Any]:
    if reason not in BOUNCE_REASONS:
        raise CliError(f"Unbekannter --reason {reason!r}; erlaubt: {', '.join(BOUNCE_REASONS)}.", 2)
    replay_ids = list(replay_msg_ids or [])
    if any(REPLAY_ID_RE.fullmatch(i) is None for i in replay_ids):
        raise CliError("--replay-msg-id enthält unzulässige Zeichen.", 2)
    pane = next((p for p in herdr.pane_list() if p.get("pane_id") == pane_id), None)
    if pane is None:
        raise CliError(f"Pane {pane_id} existiert nicht.", 4)
    if pane.get("agent") != adapter.name:
        raise CliError(f"Pane {pane_id} läuft nicht mit {adapter.name} (agent={pane.get('agent')!r}).", 4)
    switch_epoch = provider_block(load_registry(), adapter).get("switch_epoch")
    root = proc or proc_root()
    records = [r for r in scan_agy_processes(root) if r["pane_id"] == pane_id]
    if len(records) != 1:
        raise CliError(f"Pane {pane_id}: {len(records)} agy-Prozesse laut /proc, genau einer erwartet.", 4)
    rec = records[0]
    stale = switch_epoch is not None and rec["start_epoch"] < switch_epoch
    # Der 401-Grund betrifft einen laufenden Prozess mit totem Token, daher ist Veraltung dort nicht Voraussetzung.
    if not stale and not force_pane and reason != "auth_401":
        raise CliError("Prozess ist nicht veraltet; --force-pane nur bei bewusst gesetztem Ausnahmefall.", 4)

    pane_session = (pane.get("agent_session") or {}).get("value")
    proc_conv = rec["conversation_id"]
    if conversation and proc_conv and conversation != proc_conv:
        raise CliError("--conversation weicht von der Konversation des Prozesses ab.", 4)
    conv = conversation or proc_conv or pane_session
    work_dir = cwd or rec["cwd"]
    use_model = model or rec["model"]

    blockers: list[str] = []
    conv_ok = bool(conv) and CONVERSATION_UUID_RE.fullmatch(conv or "") is not None
    if not conv_ok:
        blockers.append("conversation_id nicht ermittelbar")
    elif pane_session and pane_session != conv:
        blockers.append("Pane-Session und Konversation weichen voneinander ab")
    if not work_dir:
        blockers.append("cwd nicht ermittelbar")
    elif not os.path.isdir(work_dir):
        # Ein fehlendes Verzeichnis lässt den Neustart nach /exit scheitern, die alte Sitzung wäre dann beendet.
        blockers.append("cwd_ungueltig")
    if not use_model:
        blockers.append("Modell nicht ermittelbar")
    elif not MODEL_RE.fullmatch(use_model):
        blockers.append("Modell enthält unzulässige Zeichen")
    db_bytes, brain_bytes = conversation_sizes(conv) if conv_ok else (0, 0)
    if db_bytes > BOUNCE_MAX_BYTES:
        blockers.append(f"Konversations-DB {db_bytes} Byte über Schwelle {BOUNCE_MAX_BYTES}")

    tab_id = pane.get("tab_id") or rec["tab_id"]
    label = herdr.tab_label(tab_id) if tab_id else None
    screen = herdr.screen_text(pane_id)
    draft = composer_draft(screen)
    blockers_live = live_blockers(pane.get("agent_status"), draft)
    guard, details = guard_blockers(
        adapter, pane, root=root, pid=rec["pid"], conversation=conv if conv_ok else None, label=label,
        screen=screen, switch_epoch=switch_epoch, reason=reason, allow_focused=allow_focused, source=source,
    )
    blockers += guard

    # Live-Blocker bleiben separat ausgewiesen; execute_bounce prüft sie unmittelbar vor /exit erneut.
    resumable = not blockers
    return {
        "dry_run": False,
        "provider": adapter.name,
        "pane_id": pane_id,
        "tab_id": tab_id,
        "tab_label": label,
        "protected_human_tab": human_protected_tab(label),
        "reason": reason,
        "source": source,
        "allow_focused": allow_focused,
        "replay_msg_ids": replay_ids,
        "focused": pane.get("focused"),
        "active_descendants": details["active_descendants"],
        "cooldown_remaining_s": details["cooldown_remaining_s"],
        "pane_status": pane.get("agent_status"),
        "pid": rec["pid"],
        "start_epoch": rec["start_epoch"],
        "switch_epoch": switch_epoch,
        "stale": stale,
        "force_pane": force_pane,
        "conversation_id": conv if conv_ok else None,
        "cwd": work_dir,
        "model": use_model,
        "db_bytes": db_bytes,
        "brain_bytes": brain_bytes,
        "limit_bytes": BOUNCE_MAX_BYTES,
        "composer_draft": draft,
        "live_blockers": blockers_live,
        "heal_preview": heal_preview(adapter),
        "action": "in_place_resume" if resumable else None,
        "resume_cmd": (["agy", "--conversation", conv, "--model", use_model] if resumable else None),
        "recommended_action": "in_place_resume" if resumable else "herdr-mutter-kind",
        "to_model": use_model,
        "blockers": blockers,
    }


def execute_bounce(adapter: AgyAdapter, plan: dict[str, Any], herdr: Any, *, proc: Path | None = None,
                   exit_timeout_s: float = BOUNCE_EXIT_TIMEOUT_S,
                   start_timeout_s: float = BOUNCE_START_TIMEOUT_S,
                   shell_timeout_s: float = BOUNCE_SHELL_TIMEOUT_S, poll_s: float = 0.5,
                   resume_prompt: bool = True,
                   ready_timeout_s: float = WAKEUP_READY_TIMEOUT_S,
                   work_timeout_s: float = WAKEUP_WORK_TIMEOUT_S,
                   quiet_recheck_s: float = 0.0) -> dict[str, Any]:
    if plan["recommended_action"] != "in_place_resume":
        return {**plan, "status": "refused", "recommended_action": "herdr-mutter-kind",
                "wakeup_sent": False, "wakeup_status": "not_attempted"}
    pane_id = plan["pane_id"]
    root = proc or proc_root()
    # Frische Prüfung unmittelbar vor /exit mit derselben Funktion wie der Dry-Run.
    fresh = _fresh_blockers(adapter, plan, herdr, root)
    if fresh:
        raise CliError("Live-Vorbedingung verletzt, fail-closed, kein /exit: " + "; ".join(fresh), 4)
    # Bedingung 4c: außer beim eindeutigen 401-Beleg muss die Pane über zwei Messungen unverändert ruhen.
    if plan["reason"] != "auth_401":
        first = _quiet_snapshot(herdr, pane_id, root, plan["pid"])
        if quiet_recheck_s > 0:
            time.sleep(quiet_recheck_s)
        second = _quiet_snapshot(herdr, pane_id, root, plan["pid"])
        if first != second or first[1] or second[1]:
            raise CliError("Ruhenachweis nicht bestanden (Pane-Ausgabe geändert oder Nachfahren aktiv), kein /exit.", 4)
    with switch_lock():
        heal_result = heal_locked(adapter)
    if heal_result == "unreadable":
        raise CliError("Live-Token nicht lesbar; Bounce abgebrochen.", 3)

    # Nach Ruhephase und Heal erneut prüfen: Fokus, Tab-Schutz und Prozessbindung können sich inzwischen geändert haben.
    late = _fresh_blockers(adapter, plan, herdr, root)
    if late:
        raise CliError("Live-Vorbedingung nach der Ruhephase verletzt, fail-closed, kein /exit: " + "; ".join(late), 4)

    # Versuch vor /exit persistieren, damit auch ein gescheiterter Versuch den Anti-Loop-Cap auslöst.
    # record_bounce_attempt prüft den Cooldown unter demselben Lock und lehnt einen zweiten Versuch ab.
    record_bounce_attempt(pane_id, plan["switch_epoch"])
    result = _bounce_steps(herdr, root, plan, heal_result, exit_timeout_s, start_timeout_s, shell_timeout_s, poll_s)
    result.update({"wakeup_sent": False, "wakeup_status": "not_attempted", "reason": plan["reason"],
                   "source": plan["source"], "unread_count": briefkasten_unread(pane_id)})
    if result["status"] == "ok":
        if resume_prompt:
            result.update(_wakeup(herdr, pane_id, ready_timeout_s, work_timeout_s, poll_s,
                                  replay_msg_ids=plan["replay_msg_ids"]))
        else:
            result.update({"wakeup_sent": False, "wakeup_status": "disabled"})
    log_event(
        "pane_bounced",
        provider=adapter.name,
        pane_id=pane_id,
        conversation_id=plan["conversation_id"],
        old_pid=plan["pid"],
        new_pid=result.get("new_pid"),
        status=result["status"],
        stage=result.get("stage"),
        reason=plan["reason"],
        source=plan["source"],
        wakeup_sent=result["wakeup_sent"],
        wakeup_status=result["wakeup_status"],
    )
    return result


def _fresh_blockers(adapter: AgyAdapter, plan: dict[str, Any], herdr: Any, root: Path) -> list[str]:
    """Frischprüfung unmittelbar vor /exit: Pane, Tab-Schutz, Prozessbindung und Arbeitsverzeichnis neu lesen."""
    pane = next((p for p in herdr.pane_list() if p.get("pane_id") == plan["pane_id"]), None)
    if pane is None:
        return ["Pane verschwunden"]
    screen = herdr.screen_text(plan["pane_id"])
    blockers = live_blockers(pane.get("agent_status"), composer_draft(screen))
    # Die Tab-Beschriftung wird erneut gelesen: ein im Plan ungeschützter Tab kann inzwischen umbenannt sein.
    fresh_label = herdr.tab_label(plan["tab_id"]) if plan.get("tab_id") else None
    guard, _details = guard_blockers(
        adapter, pane, root=root, pid=plan["pid"], conversation=plan["conversation_id"], label=fresh_label,
        screen=screen, switch_epoch=plan["switch_epoch"], reason=plan["reason"],
        allow_focused=plan["allow_focused"], source=plan["source"],
    )
    blockers += guard
    # Derselbe Prozess mit derselben Startzeit und Konversation muss noch allein auf dieser Pane laufen.
    on_pane = [r for r in scan_agy_processes(root) if r["pane_id"] == plan["pane_id"]]
    if len(on_pane) != 1 or (on_pane[0]["pid"], on_pane[0]["start_epoch"]) != (plan["pid"], plan["start_epoch"]):
        blockers.append("prozessbindung_geaendert")
    else:
        # Dieselben Quellen wie der Planer: ein ausdrücklich gesetztes Prozess-Argument und die Pane-Session
        # müssen zur geplanten Konversation passen. Fehlt beides, war die Konversation nie bestimmt.
        planned = plan["conversation_id"]
        pane_session = (pane.get("agent_session") or {}).get("value")
        process_conversation = on_pane[0]["conversation_id"]
        if not planned or (process_conversation and process_conversation != planned) or \
                (pane_session and pane_session != planned):
            blockers.append("konversation_geaendert")
    if not plan.get("cwd") or not os.path.isdir(plan["cwd"]):
        blockers.append("cwd_ungueltig")
    return blockers


def _quiet_snapshot(herdr: Any, pane_id: str, root: Path, pid: int) -> tuple[str, list[dict[str, Any]]]:
    return herdr.screen_text(pane_id), find_active_descendants(root, pid)


def _pane_status(herdr: Any, pane_id: str) -> str | None:
    return next((p.get("agent_status") for p in herdr.pane_list() if p.get("pane_id") == pane_id), None)


def _wakeup(herdr: Any, pane_id: str, ready_timeout_s: float, work_timeout_s: float, poll_s: float,
            replay_msg_ids: list[str] | None = None) -> dict[str, Any]:
    """Weckruf nach dem Kontowechsel: erst auf idle warten, Banner wegdrücken, dann über herdr_chat senden."""
    # Ein Fehler beim Beobachten ist getrennt vom Versand zu melden. Der Neustart ist zu diesem Zeitpunkt bereits
    # abgeschlossen, deshalb darf eine herdr-Störung hier nicht als gescheiterter Bounce erscheinen.
    sent_ok = False
    try:
        deadline = time.monotonic() + ready_timeout_s
        while _pane_status(herdr, pane_id) not in READY_PANE_STATUSES:
            if time.monotonic() > deadline:
                return {"wakeup_sent": False, "wakeup_status": "pane_not_ready"}
            time.sleep(poll_s)
        if WAKEUP_BANNER in herdr.screen_text(pane_id):
            herdr.send_keys(pane_id, "esc")
            time.sleep(poll_s)

        text = WAKEUP_TEXT
        if replay_msg_ids:
            text += " Abgebrochene Nachricht: " + ", ".join(replay_msg_ids) + "."
        chat_bin = os.environ.get("HERDR_CHAT_BIN") or HERDR_CHAT_DEFAULT
        argv = ([sys.executable] if chat_bin.endswith(".py") else []) + [
            chat_bin, "send", "--durable", "--art", "nachricht", "--betreff", WAKEUP_BETREFF, pane_id, text,
        ]
        try:
            sent = subprocess.run(argv, capture_output=True, text=True, timeout=30, check=False)
        except (OSError, subprocess.TimeoutExpired) as exc:
            return {"wakeup_sent": False, "wakeup_status": f"send_failed: {exc}"}
        if sent.returncode != 0:
            return {"wakeup_sent": False, "wakeup_status": f"send_failed: exit {sent.returncode}"}
        sent_ok = True

        deadline = time.monotonic() + work_timeout_s
        while True:
            if _pane_status(herdr, pane_id) == "working":
                return {"wakeup_sent": True, "wakeup_status": "working_seen"}
            if time.monotonic() > deadline:
                return {"wakeup_sent": True, "wakeup_status": "no_working_seen"}
            time.sleep(poll_s)
    except CliError as exc:
        return {"wakeup_sent": sent_ok, "wakeup_status": f"beobachtung_fehlgeschlagen: {exc}"}


def _bounce_steps(herdr, root, plan, heal_result, exit_timeout_s, start_timeout_s, shell_timeout_s, poll_s):
    pane_id = plan["pane_id"]
    conv = plan["conversation_id"]
    herdr.run(pane_id, "/exit")
    deadline = time.monotonic() + exit_timeout_s
    while (root / str(plan["pid"])).exists():
        if time.monotonic() > deadline:
            return {**plan, "status": "failed", "stage": "exit", "heal": heal_result,
                    "recommended_action": "herdr-mutter-kind"}
        time.sleep(poll_s)
    leftover = [r for r in scan_agy_processes(root) if r["pane_id"] == pane_id]
    if leftover:
        return {**plan, "status": "failed", "stage": "exit", "heal": heal_result,
                "recommended_action": "herdr-mutter-kind", "leftover_pids": [r["pid"] for r in leftover]}

    deadline = time.monotonic() + shell_timeout_s
    while True:
        try:
            names = herdr.foreground_names(pane_id)
        except CliError as exc:
            return {**plan, "status": "failed", "stage": "shell", "heal": heal_result, "error": str(exc),
                    "recommended_action": "herdr-mutter-kind"}
        if names and all(name in SHELL_NAMES for name in names):
            break
        if time.monotonic() > deadline:
            return {**plan, "status": "failed", "stage": "shell", "heal": heal_result, "foreground": names,
                    "recommended_action": "herdr-mutter-kind"}
        time.sleep(poll_s)

    try:
        herdr.run(pane_id, f"cd {_shell_quote(plan['cwd'])} && agy --conversation {_shell_quote(conv)} --model {_shell_quote(plan['model'])}")
    except CliError as exc:
        # Der alte Prozess ist bereits beendet. Ein Fehler beim Senden wird als Ergebnis gemeldet, nicht erneut gesendet:
        # herdr kann den Befehl trotz Fehlers angenommen haben.
        return {**plan, "status": "failed", "stage": "start", "heal": heal_result, "error": str(exc),
                "recommended_action": "herdr-mutter-kind"}
    deadline = time.monotonic() + start_timeout_s
    while True:
        new = [r for r in scan_agy_processes(root) if r["pane_id"] == pane_id and r["conversation_id"] == conv]
        if new:
            return {**plan, "status": "ok", "stage": "start", "heal": heal_result, "new_pid": new[0]["pid"],
                    "recommended_action": "in_place_resume"}
        if time.monotonic() > deadline:
            return {**plan, "status": "failed", "stage": "start", "heal": heal_result,
                    "recommended_action": "herdr-mutter-kind"}
        time.sleep(poll_s)


def _shell_quote(value: str) -> str:
    return shlex.quote(value)


def cmd_stale_panes(adapter: AgyAdapter, as_json: bool) -> str:
    reg = load_registry()
    switch_epoch = provider_block(reg, adapter).get("switch_epoch")
    panes = find_stale_panes(proc_root(), switch_epoch)
    if as_json:
        return json.dumps(panes, indent=2, ensure_ascii=False)
    return "\n".join(f"pid {p['pid']} pane {p['pane_id']} ({p['cmdline_kind']})" for p in panes) or "Keine veralteten Panes."


# ---------------------------------------------------------------------------
# install-watcher: systemd-user-Units schreiben, nicht aktivieren
# ---------------------------------------------------------------------------


def watcher_units(adapter: AgyAdapter) -> dict[str, str]:
    base = f"llm-account-{adapter.name}-heal"
    path_unit = (
        "[Unit]\n"
        f"Description=llm-account: Wächter für das {adapter.name}-Live-Token\n\n"
        "[Path]\n"
        f"PathChanged=%h/{adapter.token_rel.as_posix()}\n\n"
        "[Install]\n"
        "WantedBy=default.target\n"
    )
    service_unit = (
        "[Unit]\n"
        f"Description=llm-account: Heal für {adapter.name}\n\n"
        "[Service]\n"
        "Type=oneshot\n"
        f"ExecStart=%h/.local/bin/llm-account heal {adapter.name}\n"
    )
    return {f"{base}.path": path_unit, f"{base}.service": service_unit}


def cmd_install_watcher(adapter: AgyAdapter, dry_run: bool, unit_dir: str | None) -> str:
    units = watcher_units(adapter)
    if dry_run:
        return "\n".join(f"--- {name} ---\n{content}" for name, content in units.items())
    target = Path(unit_dir or DEFAULT_UNIT_DIR).expanduser()
    target.mkdir(parents=True, exist_ok=True)
    for name, content in units.items():
        atomic_write(target / name, content.encode("utf-8"), mode=0o644)
    names = " ".join(units)
    return (
        f"Units geschrieben nach {target}. Nicht aktiviert; aktivieren mit: "
        f"systemctl --user enable --now {names.split()[0]}"
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="llm-account", description="Kontowechsel für LLM-Provider (N >= 1).")
    sub = parser.add_subparsers(dest="verb", required=True)

    p = sub.add_parser("save", help="Live-Konto in einen Slot sichern")
    p.add_argument("provider")
    p.add_argument("slot")
    p.add_argument("--label")

    p = sub.add_parser("login", help="Neues Konto isoliert anmelden (aktives Konto bleibt)")
    p.add_argument("provider")
    p.add_argument("slot")
    p.add_argument("--label")

    p = sub.add_parser("use", help="Slot als aktives Konto aktivieren")
    p.add_argument("provider")
    p.add_argument("slot")

    p = sub.add_parser("retire", help="Slot aus der Rotation nehmen")
    p.add_argument("provider")
    p.add_argument("slot")
    p.add_argument("--force", action="store_true")

    p = sub.add_parser("heal", help="Live-Token gegen Clobbering prüfen und reparieren")
    p.add_argument("provider")

    p = sub.add_parser("status", help="Quotas aller aktiven Slots abfragen")
    p.add_argument("provider", nargs="?", default="agy")
    p.add_argument("--json", action="store_true")
    p.add_argument("--write-quotas", action="store_true")

    p = sub.add_parser("stale-panes", help="agy-Prozesse vor dem letzten Wechsel auflisten")
    p.add_argument("provider")
    p.add_argument("--json", action="store_true")

    p = sub.add_parser("install-watcher", help="systemd-user-Wächter-Units schreiben (nicht aktivieren)")
    p.add_argument("provider")
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--unit-dir")

    p = sub.add_parser("bounce-stale", help="In-Place-Warm-Bounce einer Pane mit veraltetem agy-Prozess")
    p.add_argument("provider")
    p.add_argument("pane_id")
    p.add_argument("--dry-run", action="store_true", help="nur Plan als JSON ausgeben, nichts verändern")
    p.add_argument("--yes", action="store_true", help="Live-Eingriff ausdrücklich freigeben")
    p.add_argument("--conversation")
    p.add_argument("--cwd")
    p.add_argument("--model")
    p.add_argument("--force-pane", action="store_true")
    p.add_argument("--no-resume-prompt", action="store_true", help="Weckruf nach dem Wechsel auslassen")
    p.add_argument("--reason", choices=BOUNCE_REASONS, default="switch_epoch",
                   help="Grund des Eingriffs; auth_401 erlaubt den In-Place-Warm-Bounce auch auf Architekten-Tabs")
    p.add_argument("--quelle", help="Zielbindung: Nachrichten-ID oder Vertragsname (Pflicht bei --reason auth_401)")
    p.add_argument("--allow-focused", action="store_true", help="nur ausdrücklich benannte fokussierte Pane")
    p.add_argument("--replay-msg-id", action="append", default=[], dest="replay_msg_ids",
                   help="abgebrochene Briefkasten-Nachricht im Weckruf nennen (wiederholbar)")
    return parser


def run(args: argparse.Namespace) -> str:
    adapter = get_adapter(args.provider)
    validate_agy_binary_override()  # vor jedem Vorgang, der agy startet
    if args.verb == "save":
        return cmd_save(adapter, args.slot, args.label)
    if args.verb == "login":
        return cmd_login(adapter, args.slot, args.label)
    if args.verb == "use":
        return cmd_use(adapter, args.slot)
    if args.verb == "retire":
        return cmd_retire(adapter, args.slot, args.force)
    if args.verb == "heal":
        return cmd_heal(adapter)
    if args.verb == "status":
        return cmd_status(adapter, args.json, args.write_quotas)
    if args.verb == "stale-panes":
        return cmd_stale_panes(adapter, args.json)
    if args.verb == "install-watcher":
        return cmd_install_watcher(adapter, args.dry_run, args.unit_dir)
    if args.verb == "bounce-stale":
        return cmd_bounce_stale(adapter, args)
    raise CliError(f"Unbekanntes Verb {args.verb!r}.", 2)


def cmd_bounce_stale(adapter: AgyAdapter, args: argparse.Namespace, herdr: Any = None) -> str:
    herdr = herdr or HerdrCli()
    # getattr mit Standardwerten: ältere Aufrufer mit schmalem Namespace bleiben gültig.
    plan = plan_bounce(
        adapter, args.pane_id, herdr,
        conversation=args.conversation, cwd=args.cwd, model=args.model, force_pane=args.force_pane,
        reason=getattr(args, "reason", "switch_epoch"),
        allow_focused=getattr(args, "allow_focused", False),
        source=getattr(args, "quelle", None),
        replay_msg_ids=getattr(args, "replay_msg_ids", None),
    )
    if args.dry_run:
        plan["dry_run"] = True
        return json.dumps(plan, indent=2, ensure_ascii=False)
    if not args.yes:
        raise CliError("Live-Eingriff nur mit --yes; zur Prüfung zuerst --dry-run verwenden.", 2)
    return json.dumps(
        execute_bounce(adapter, plan, herdr, resume_prompt=not args.no_resume_prompt, quiet_recheck_s=QUIET_RECHECK_S),
        indent=2, ensure_ascii=False,
    )


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        output = run(args)
    except CliError as exc:
        print(f"llm-account: {exc}", file=sys.stderr)
        return exc.exit_code
    except KeyboardInterrupt:
        return 130
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
