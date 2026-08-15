> [Zurück zum Index](./INDEX.md) | **Systembild** | [Bedienung und Zustände](02-bedienung-und-zustaende.md) | [Betrieb und Fehlerdiagnose](03-betrieb-und-fehlerdiagnose.md) | [Entwicklung und Test](04-entwicklung-und-test.md)

# Systembild

> **Zweck:** Eine spätere Änderung soll an der richtigen Sicherheitsgrenze ansetzen.
> **Quellstand:** 2026-08-04

---

## Das Sicherheitsmodell zuerst

Der Nachtwächter trifft keine Aussage darüber, ob ein Terminal „leer aussieht“. Er bewertet fortlaufend alle Agenten, die Herdr aktuell meldet. Die Python-Datei [`herdr-night-watch.py`](../watcher/herdr-night-watch.py) ist damit die maßgebliche Sicherheitsinstanz. Sie hält Zustand und Protokoll in WSL, prüft Herdr und löst als einziges Bauteil einen echten Windows-Shutdown aus.

Die Rust-Anwendung in diesem Projekt ist dagegen ein Controller. Sie bietet die Bedienung in der Windows-Taskleiste, zeigt Status und kann den Wächter starten oder stoppen. Im Live-Fenster wählt der kleine Schalter rechts neben „Herdr jetzt“, ob ein späterer Abschluss Windows in den Energiesparmodus versetzt oder herunterfährt. Diese Wahl wird beim Start eines Nachtlaufs fest gespeichert. Ihre Trennung vom Wächter ist gewollt: Die App darf beendet, aktualisiert oder neu gestartet werden, ohne dass dadurch die überwachte Arbeitsmenge oder die festgelegte Abschlussaktion verloren geht.

## Komponenten und Verantwortlichkeiten

| Bereich | Datei oder Ort | Verantwortung |
|---|---|---|
| Tray-App | [`src/tray.rs`](../src/tray.rs) | Menü, Symbol, periodische Statusanzeige und Dialogauslösung |
| Windows-Integration | [`src/backend.rs`](../src/backend.rs) | Verdeckte WSL-Aufrufe, Start des Hintergrundwächters und Statusübersetzung |
| Lokale Einrichtung | [`src/configuration.rs`](../src/configuration.rs) und [`src/settings.rs`](../src/settings.rs) | WSL-Distro und Wächterpfad lokal speichern und im Setup-Fenster bearbeiten |
| Warnfenster | [`src/notify.rs`](../src/notify.rs) | Windows-Dialog mit „Abbrechen“ in der echten Warnphase |
| Live-Status | [`src/live_status.rs`](../src/live_status.rs) | Frei platzierbares, schließbares Fenster für Live-Arbeit und Nachtlaufzustand |
| Wetteranzeige | [`src/weather.rs`](../src/weather.rs) und [`src/weather_location.rs`](../src/weather_location.rs) | Hintergrund-Temperaturabruf und Suchfenster für den lokalen Wetterort |
| Autostart | [`src/autostart.rs`](../src/autostart.rs) | `HKCU\\...\\Run` für die Tray-App, nie für einen Nachtlauf |
| Wächter | [`herdr-night-watch.py`](../watcher/herdr-night-watch.py) | Prüfung aller aktuellen Herdr-Agenten, Ruhezeit, Shutdown und Abbruch |
| Windows-Skripte | [`windows/`](../windows/) | Manuelle Diagnose- und Kompatibilitätshilfen |

## Ablauf eines echten Nachtlaufs

Ein Nachtlauf beginnt in Windows und wird in WSL entschieden. Die Trennung ist wichtig: Nur der Wächter besitzt den langlebigen Zustand, die App ist jederzeit austauschbar.

```text
Tray: „Nachtmodus starten“
        |
        v
Rust-Backend: WSL-Aufruf mit --arm
        v
WSL: herdr-night-watch.py schreibt active-run.json
        |  Prüfbereich „alle aktuellen Herdr-Agenten“
        v
Rust-Backend startet `wsl.exe --watch` als fensterlosen Hintergrundprozess
        |
        v
Wächter prüft aktuellen Herdr-Status, Ruhezeit und Warnfrist
        |
        +-- Nachtlauf aktiv -> Tray setzt Windows-Ausführungsstatus gegen automatischen Leerlauf-Energiesparmodus
        |
        +-- Unsicherheit/blocked/fehlender Status -> refused, kein Shutdown
        |
        +-- Internet fünf Minuten nicht erreichbar -> normale Warnfrist, bei Rückkehr Abbruch
        |
        +-- keine Herdr-Arbeit lange genug aktiv -> gewählte Warnfrist, danach Energiesparmodus oder shutdown.exe
                                                |
                                                v
                                      Tray zeigt Dialog; Abbrechen -> --cancel
```

## Agentenunabhängige Anbindung

Der Nachtwächter hängt nicht an Codex oder Claude Code. Beide sind lediglich mögliche Programme in Herdr-Panes. Entscheidend ist allein die Herdr-CLI: Sie meldet dem Wächter, ob aktuell Arbeit läuft. Der Wächter liest weder Prompt-Inhalte noch Terminalausgaben und steuert keine Agenten. Damit kann eine Herdr-Sitzung auch Codex, Claude Code und weitere Agenten parallel enthalten.

Die öffentliche Anwendung speichert zwei lokale Angaben unter dem Windows-Benutzerkonto: den Namen der WSL-Distro und den vollständigen WSL-Pfad zur Datei `herdr-night-watch.py`. Sie stehen im Rechtsklick-Menü unter „Einrichtung öffnen“ zur Verfügung. Das ist bewusst kein fest eingebauter Codex-Pfad mehr und gehört nicht in Git oder Logs.

Der Wetterort wird zusätzlich als lokale Windows-Registry-Einstellung gespeichert. Standardmäßig ist Leipzig hinterlegt. Die Live-App fragt die Temperatur im Hintergrund über Open-Meteo ab und zeigt bei fehlender Verbindung keinen erfundenen Wert. Diese Anzeige ist vollständig vom Wächter getrennt und kann keine Energieaktion auslösen.

Auch die Position des Live-Fensters wird lokal unter dem Windows-Benutzerkonto gespeichert. Beim erneuten Öffnen wird ein vorhandenes Fenster restauriert und nach vorn geholt; der Startpfad schützt zusätzlich mit einer benannten Windows-Sperre vor doppelten Live-Fenstern. Fehlschläge beim Öffnen werden unter `logs/ui-errors.log` protokolliert.

## Persistenter Zustand und Protokoll

Der Wächter legt seinen Zustand unter `~/.local/state/herdr-night-watch/` in der WSL-Distribution ab. Dieser Ort ist entscheidend für Diagnose und darf nicht ohne Grund gelöscht werden.

| Datei | Bedeutung |
|---|---|
| `active-run.json` | Scharfgestellter oder letzter abgeschlossener Lauf, inklusive Prüfbereich, Startwert und Ergebnis |
| `shutdown-warning.json` | Existiert nur während einer eigenen widerrufbaren Shutdown-Warnung |
| `settings.json` | Bevorzugte Abschlussaktion und Warnfrist für den nächsten Nachtlauf: `sleep` oder `shutdown`, 10 bis 3.600 Sekunden |
| `watch.lock` | Verhindert zwei gleichzeitige Wächterprozesse |
| `watch.log` | Zeitstempel-Protokoll aller wichtigen Entscheidungen |
| Windows-Installationsordner `logs\\completion-history.csv` | Letzte 30 angeforderte Energiespar- oder Shutdown-Vorgänge mit lokaler Uhrzeit und Auslöser |
| Windows-Installationsordner `logs\\cancellation-history.csv` | Letzte 30 manuell oder vom Warnfenster abgebrochene Nachtläufe mit genauer Abbruchquelle |

Neue Läufe speichern `monitoring_scope=live_agents` und die Zahl der beim Start arbeitenden Agenten nur zur Nachvollziehbarkeit. Entscheidend ist danach immer die aktuelle Herdr-Antwort: Solange mindestens ein Agent `working` meldet, läuft der Nachtmodus weiter. Startet während der Ruhezeit oder Warnfrist neue Arbeit, wird die eigene Warnung abgebrochen und der Wächter beobachtet weiter. Alte Zustandsdateien ohne diesen Prüfbereich bleiben aus Kompatibilitätsgründen bei ihrer ursprünglichen Snapshot-Prüfung.

Die Abschlussaktion und die Warnfrist werden beim Start zusätzlich in `active-run.json` eingefroren. Damit kann ein Klick im Live-Fenster keinen schon gestarteten Ablauf verändern. Der Schalter und das Sekundenfeld sind deshalb während eines aktiven Nachtlaufs bewusst gesperrt. Im Modus `sleep` läuft dieselbe gewählte Warnfrist ab; danach fordert der Wächter Windows zum Energiesparen auf. Im Modus `shutdown` wird der bekannte Windows-Shutdown angesetzt.

Solange der Nachtlauf den Zustand `Watching` oder `ShutdownWarning` meldet, setzt die Tray-App den Windows-Ausführungsstatus `ES_SYSTEM_REQUIRED`. Dadurch darf ein konfigurierter Leerlauf-Timer den Rechner nicht ungefragt in den Energiesparmodus schicken, während Herdr noch überwacht wird. Der Bildschirm darf sich weiterhin ausschalten. Die Sperre wird beim Stoppen, beim Ende des Nachtlaufs und beim Beenden der Tray-App wieder freigegeben. Eine vom Wächter selbst bestätigte Energieaktion bleibt davon unberührt.

Für Neustarts vergleicht der Wächter nicht nur die WSL-Boot-ID, sondern zusätzlich die letzte Windows-Bootzeit. Ändert sich eine der beiden Marken, gilt der alte Lauf als veraltet: Warnung und aktiver Zustand werden zurückgesetzt und als `system_restart` protokolliert.

Der Wächter prüft zusätzlich zwei unabhängige Internet-Endpunkte. Erst wenn beide fünf Minuten durchgehend nicht erreichbar sind, startet er dieselbe sichtbare Warnfrist wie bei fertig gemeldeten Agenten. Kommt die Verbindung während dieser Frist zurück, bricht er seine eigene Warnung ab und überwacht weiter. Jeder tatsächliche Abschluss wird als angeforderter Energiesparmodus oder Shutdown in `completion-history.csv` aufgezeichnet; beim 31. Eintrag fällt der älteste der 30 bisherigen Einträge heraus.

Abbrüche erhalten ebenfalls einen eigenen Verlauf. `cancellation-history.csv` nennt als Quelle zum Beispiel `live_window_moon`, `tray_menu`, `warning_dialog`, `manual_stop_script` oder `start_failed`. Damit ist am Folgetag nachvollziehbar, warum ein Nachtlauf nicht mehr aktiv war.

`live_agent_summary()` liefert dieselbe aktuelle Herdr-Sicht für Tooltip und Live-Fenster. Die Shutdown-Entscheidung ruft Herdr unabhängig erneut ab und vertraut nie auf eine alte Anzeige. Ist Herdr nicht erreichbar oder ein Agentenstatus unvollständig, wird kein Shutdown vorbereitet.

## Die zwei Wahrheiten: Quellcode und Auslieferung

Der bearbeitbare Rust-Quellcode liegt im Repository. Die EXE wird in einen frei wählbaren Windows-Installationsordner kopiert.

Eine Quellcode-Änderung wirkt deshalb noch nicht automatisch in der laufenden Tray-App. Erst Windows-Build, Kopie der Release-Datei an den Installationsort und ein kontrollierter Neustart der Tray-App machen sie sichtbar. Die Details und die sichere Reihenfolge stehen in [Entwicklung und Test](./04-entwicklung-und-test.md).
