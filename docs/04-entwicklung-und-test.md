> [Zurück zum Index](./INDEX.md) | [Systembild](01-systembild.md) | [Bedienung und Zustände](02-bedienung-und-zustaende.md) | [Betrieb und Fehlerdiagnose](03-betrieb-und-fehlerdiagnose.md) | **Entwicklung und Test**

# Entwicklung und Test

> **Zweck:** Änderungen am Komfort dürfen die Fail-closed-Grenze nicht unbemerkt schwächen.
> **Quellstand:** 2026-08-04

---

## Wo eine Änderung hingehört

Der wichtigste Schritt vor einer Änderung ist die Verantwortlichkeit zu wählen. Eine Text- oder Menüänderung gehört meist nach `src/tray.rs`; die Abfrage von WSL oder das Starten verdeckter Windows-Prozesse nach `src/backend.rs`; der eigentliche Sicherheitspfad nach `watcher/herdr-night-watch.py`. Eine Änderung im Tray darf nicht stillschweigend die Kriterien verändern, mit denen der Wächter einen Shutdown erlaubt.

Besonders kritisch sind Änderungen an `arm()`, `evaluate_live_agents()`, `evaluate()`, `schedule_shutdown()` und `abort_shutdown_if_ours()` im Python-Wächter. Diese Funktionen legen fest, welche aktuelle Arbeit zählt, wann Unsicherheit vorliegt und wann Windows herunterfahren oder abbrechen darf. Vor einer solchen Änderung erst den in [Systembild](./01-systembild.md) beschriebenen Ablauf gegenlesen und den Demo- und Stopp-Pfad mitdenken.

## Sicherheitsinvarianten

Diese Regeln sind Testspezifikation, nicht bloß Beschreibung:

- Jeder aktuell von Herdr gemeldete Agent wird in jeder Prüfung ausgewertet; ein neuer `working`-Agent setzt Ruhezeit oder Warnfrist zurück.
- `idle` und `done` sind erst nach einer ununterbrochenen Ruhezeit ohne irgendeinen `working`-Agenten terminal.
- `blocked`, `unknown`, ein fehlender Status oder eine nicht lesbare Herdr-Antwort führen zu keinem Shutdown.
- Vor dem Ansetzen des Shutdowns findet eine unmittelbare zweite Prüfung statt.
- Wird während der Warnfrist Arbeit wieder aktiv, wird der eigene Shutdown abgebrochen und der Nachtlauf beobachtet weiter.
- `--demo` und `--dry-run` dürfen niemals eine Windows-Energieaktion ausführen - auch nicht über eine Sofortbestätigung.
- Die Abschlussaktion wird beim Start eines Nachtlaufs eingefroren; ein späterer UI-Klick darf den aktiven Lauf nicht umdeuten.
- Die Warnfrist wird ebenso eingefroren. Das Sekundenfeld erlaubt nur 10 bis 3.600 Sekunden.
- Ein Start ohne explizites `-WarningSeconds` übernimmt den im Live-Fenster gespeicherten Wert; ein Skript-Default darf ihn nicht auf 300 zurücksetzen.
- Energiesparmodus darf weder `shutdown.exe /s` noch `shutdown.exe /a` ausführen.
- Nur eine positive Benutzerbestätigung der eigenen aktiven Warnung darf Energiesparmodus oder Herunterfahren vor Ablauf der Warnfrist auslösen.
- Internet-Ausfall löst erst nach fünf Minuten und nur über dieselbe sichtbare Warnfrist aus; Rückkehr der Verbindung bricht diese Warnung ab.
- `completion-history.csv` darf höchstens 30 Ereignisse enthalten und protokolliert nur tatsächlich angeforderte Aktionen.
- `cancellation-history.csv` darf höchstens 30 Ereignisse enthalten und muss die konkrete Abbruchquelle speichern.
- `--cancel` darf nur einen Shutdown abbrechen, für den eine gültige eigene Warnungsdatei existiert.
- Die Tray-App und ihre Hilfsprozesse dürfen keine sichtbaren Konsolenfenster erzeugen.
- `diagnostics.jsonl` muss für jede relevante Zustandsänderung maschinenlesbare Ereignisse liefern und auf die letzten 500 Einträge begrenzt bleiben.
- Eine neue Wächterinstanz darf erst nach Freigabe des Lock des Vorgängers prüfen.
- Tooltip und Live-Fenster zeigen die aktuelle Herdr-Sicht; die Shutdown-Prüfung liest diesen Status unabhängig erneut.

## Build und Auslieferung

Der Rust-Quellcode lebt im Repository. Die EXE wird in einen frei wählbaren Windows-Installationsordner kopiert.

Die sichere Reihenfolge lautet: Quellcode prüfen, Rust formatieren und testen, Windows-Build durchführen, die erzeugte EXE gezielt an den Installationsort kopieren und erst dann die Tray-App neu starten. Während eines echten aktiven Nachtlaufs keine Auslieferung vornehmen: Die App kann zwar unabhängig vom Wächter ersetzt werden, aber die Warn- und Bedienungslogik sollte nicht mitten in einer laufenden Warnfrist wechseln.

Beispiel für den WSL-seitigen Quellcheck:

```bash
cd <Repository>
cargo fmt --check
cargo check --target x86_64-pc-windows-gnu
cargo test --target x86_64-pc-windows-gnu
python3 -m py_compile watcher/herdr-night-watch.py
```

Der tatsächliche Release-Build muss unter Windows mit der vorhandenen MSVC-Umgebung laufen, zum Beispiel aus einer Windows-PowerShell:

```powershell
Set-Location <WindowsRepository>
cargo build --release
```

Danach den genauen Release-Pfad prüfen und die EXE kontrolliert in den Installationsordner kopieren. Nie blind einen ganzen Ordner überschreiben und nie die aktive Anwendung ohne vorherigen Prozess- und Nachtlauf-Status ersetzen.

## Testmatrix

Ein echter Windows-Shutdown wird nicht als automatischer Test ausgeführt. Die Testabdeckung entsteht aus dem sicheren Beobachtungsmodus, der Demo und gezielten Statusprüfungen.

| Änderung | Pflichtprüfung | Erfolgszeichen |
|---|---|---|
| Tray-Text, Menü oder Symbol | App starten, Menü und Tooltip ansehen | Kein zusätzliches Fenster, passende Menü-Sperren |
| Live-Status | Mehrere Herdr-Agenten laufen lassen, Live-Status öffnen | Live-Zahl, großer Mond mit Hover-Hinweis sowie funktionierender Start- oder Stoppknopf sind sichtbar; Abschluss-Schalter und Sekundenfeld liegen rechts neben „Herdr jetzt“, speichern nur außerhalb eines aktiven Laufes; bei Warnfrist zeigt das Fenster einen roten Countdown; Fenster ist beweglich und schließbar |
| WSL-/PowerShell-Start | Beobachtungsmodus mit echter Herdr-Arbeit | Task läuft, Status hat `monitoring_scope=live_agents`, kein Shutdown |
| Warnfenster | `Demo: Abschluss simulieren` | Ruhezeit, rotes Symbol und Dialog sichtbar; Windows bleibt an |
| Stopp-Pfad | Demo oder Beobachtung starten, dann Stopp | Task beendet, Log enthält `CANCELLED` oder Abschluss |
| Wächter-Sicherheitslogik | Neue Herdr-Arbeit oder gezielt unklare Herdr-Situation | Ruhezeit/Warnfrist wird zurückgesetzt oder Ergebnis `refused`, niemals Shutdown |
| Internet-Ausfall | Beide Verbindungschecks fünf Minuten unerreichbar simulieren, dann Verbindung zurückgeben | Normale Warnfrist startet mit Internet-Hinweis; Rückkehr bricht sie ab |
| Abschlussverlauf | 31 Abschlussereignisse in Testumgebung erzeugen | `logs\\completion-history.csv` hat Header plus maximal 30 jüngste Einträge |
| Abbruchverlauf | Nachtlauf über Mond, Menü, Warnfenster und Stoppskript abbrechen | `logs\\cancellation-history.csv` nennt die passende Abbruchquelle |
| Verdeckter Start | Task über Tray starten und Desktop beobachten | Kein aufblitzendes Terminalfenster |
| Standard-Countdown | Nachtmodus mit keiner mehr arbeitenden Herdr-Arbeit | Nach 5 Sekunden beginnt ein abbrechbarer 300-Sekunden-Countdown; das Feld im Live-Fenster kann für den nächsten Lauf 10 bis 3.600 Sekunden festlegen |
| Gespeicherte Warnfrist | Im Live-Fenster 10 Sekunden speichern, danach per Tray oder Startskript ohne Parameter starten | `active-run.json` enthält `warning_seconds=10` |

## Prüfen vor dem Neustart der Tray-App

Vor einem Neustart ist der Status wichtig. In Windows zuerst `Get-HerdrNightWatchStatus.ps1` ausführen. Ist der Task `Running` oder liegt eine Warnfrist vor, keine unnötige App-Aktualisierung vornehmen. Ist der Lauf beendet oder aus, kann die alte Tray-App sauber beendet und die neue EXE gestartet werden.

Nach dem Neustart prüft die App beim Start `backend::status()`. Fällt dieser Aufruf fehl, zeigt sie zunächst „Aus“ und meldet den Fehler im Menü. Das ist eine Anzeigegrenze, keine Freigabe zum manuellen Shutdown. In diesem Fall zuerst den WSL-Status und das Log prüfen.

## Dokumentation mitpflegen

Bei jeder fachlichen Änderung dieses Systems zuerst prüfen, ob eine Sicherheitsinvariante, ein Menüeintrag, ein Standardwert, ein Pfad oder ein Diagnoseweg betroffen ist. Dann die passende Seite in diesem Ordner und [CHANGELOG.md](./CHANGELOG.md) aktualisieren. Die Dokumentation soll nicht das Marketingbild des Tools sein, sondern einem späteren Bearbeiter zuverlässig sagen, welche Änderung sicher ist und welche nicht.
