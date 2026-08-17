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
- `completion-history.csv` protokolliert die angeforderten Energieaktionen des WSL-Wächters. `tray-history.csv` protokolliert erkannte unplanmäßige Tray-Enden. Der Log-Viewer zeigt beide Dateien in getrennten Bereichen und höchstens 30 Einträge je Bereich.
- `cancellation-history.csv` darf höchstens 30 Ereignisse enthalten und muss die konkrete Abbruchquelle speichern.
- `--cancel` darf nur einen Shutdown abbrechen, für den eine gültige eigene Warnungsdatei existiert.
- Die Tray-App und ihre Hilfsprozesse dürfen keine sichtbaren Konsolenfenster erzeugen.
- `diagnostics.jsonl` muss für jede relevante Zustandsänderung maschinenlesbare Ereignisse liefern und auf die letzten 500 Einträge begrenzt bleiben.
- Eine neue Wächterinstanz darf erst nach Freigabe des Lock des Vorgängers prüfen.
- Der Wächter darf nach einem Neustart erst überwachen, wenn eine vollständige WSL-/Windows-Boot-Marke bestätigt wurde; Reset und Watch müssen dieselbe bestätigte Marke verwenden.
- Tooltip und Live-Fenster zeigen die aktuelle Herdr-Sicht; die Shutdown-Prüfung liest diesen Status unabhängig erneut.
- Die Systemtelemetrie im Live-Fenster ist reine Anzeige. Fehler oder fehlende Werte dürfen den Wächter und seine Energieentscheidung nicht beeinflussen.
- Das visuelle Fenster-Chrome liegt zentral in `src/window_chrome.rs`; insbesondere darf die Transparenzsuche nie ein gleichnamiges Fenster eines anderen Prozesses verändern.

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

### Windows-Energieschutz-Smoke-Test

Der Tray aktiviert während eines Nachtlaufs einen Windows-Energieschutz gegen automatischen Leerlauf-Schlaf. Der manuelle Smoke-Test prüft den echten Windows-Pfad: Er setzt den Schutz für den aktuellen PowerShell-Prozess, sucht ihn mit `powercfg.exe /requests` in der `SYSTEM`-Sektion und gibt ihn anschließend wieder frei. Der Test fordert weder Energiesparmodus noch Herunterfahren an.

Aus einer als Administrator gestarteten PowerShell im Repository:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& .\windows\Test-HerdrNightWatchPowerGuard.ps1
```

Erwartet werden zwei `PASS`-Zeilen. Schlägt der Test fehl, bleibt keine absichtlich gesetzte Ausführungsanforderung zurück; die Freigabe liegt zusätzlich in einem `finally`-Block.

## Testmatrix

Ein echter Windows-Shutdown wird nicht als automatischer Test ausgeführt. Die Testabdeckung entsteht aus dem sicheren Beobachtungsmodus, der Demo und gezielten Statusprüfungen.

| Änderung | Pflichtprüfung | Erfolgszeichen |
|---|---|---|
| Tray-Text, Menü oder Symbol | App starten, Menü und Tooltip ansehen | Kein zusätzliches Fenster, passende Menü-Sperren |
| Live-Status | Mehrere Herdr-Agenten laufen lassen, Live-Status öffnen | Live-Zahl, großer Mond mit echter Mondphase und Hover-Hinweis sowie funktionierender Start- oder Stoppknopf sind sichtbar; die Temperatur bleibt in der dunklen Mondfläche lesbar; Abschluss-Schalter und Sekundenfeld liegen rechts neben „Herdr jetzt“, speichern nur außerhalb eines aktiven Laufes; bei Warnfrist zeigt das Fenster einen roten Countdown; die schmale Fußzeile zeigt CPU, GPU, belegten VRAM, RAM und einen verfügbaren Grafikkarten-Wattwert oder `—`; Fenster ist beweglich und schließbar |
| WSL-/PowerShell-Start | Beobachtungsmodus mit echter Herdr-Arbeit | Task läuft, Status hat `monitoring_scope=live_agents`, kein Shutdown; bei einem Boot-Rennen wartet der Wächter auf beide Boot-Marken |
| Warnfenster | `Demo: Abschluss simulieren` | Ruhezeit, rotes Symbol und Dialog sichtbar; Windows bleibt an |
| Stopp-Pfad | Demo oder Beobachtung starten, dann Stopp | Task beendet, Log enthält `CANCELLED` oder Abschluss |
| Wächter-Sicherheitslogik | Neue Herdr-Arbeit oder gezielt unklare Herdr-Situation | Ruhezeit/Warnfrist wird zurückgesetzt oder Ergebnis `refused`, niemals Shutdown |
| Internet-Ausfall | Beide Verbindungschecks fünf Minuten unerreichbar simulieren, dann Verbindung zurückgeben | Normale Warnfrist startet mit Internet-Hinweis; Rückkehr bricht sie ab |
| Abschlussverlauf | 31 Abschlussereignisse in Testumgebung erzeugen | `logs\\completion-history.csv` und `logs\\tray-history.csv` bleiben getrennt; der Viewer zeigt maximal 30 jüngste Einträge |
| Abbruchverlauf | Nachtlauf über Mond, Menü, Warnfenster und Stoppskript abbrechen | `logs\\cancellation-history.csv` nennt die passende Abbruchquelle |
| Verdeckter Start | Task über Tray starten und Desktop beobachten | Kein aufblitzendes Terminalfenster |
| Standard-Countdown | Nachtmodus mit keiner mehr arbeitenden Herdr-Arbeit | Nach 5 Sekunden beginnt ein abbrechbarer 300-Sekunden-Countdown; das Feld im Live-Fenster kann für den nächsten Lauf 10 bis 3.600 Sekunden festlegen |
| Gespeicherte Warnfrist | Im Live-Fenster 10 Sekunden speichern, danach per Tray oder Startskript ohne Parameter starten | `active-run.json` enthält `warning_seconds=10` |
| Windows-Energieschutz | Als Administrator `windows\Test-HerdrNightWatchPowerGuard.ps1` ausführen | Der aktuelle PowerShell-Prozess erscheint während des Tests in `powercfg /requests`; danach ist der Request freigegeben |
| Tray-Absturz-Erkennung | Tray starten, Prozess während eines inaktiven Zustands gezielt beenden, Tray neu starten | `logs\\tray-history.csv` enthält genau einen Hinweis „Tray-App unplanmäßig beendet“; ein erwarteter Energiesparmodus erzeugt keinen solchen Hinweis |

## Prüfen vor dem Neustart der Tray-App

Vor einem Neustart ist der Status wichtig. In Windows zuerst `Get-HerdrNightWatchStatus.ps1` ausführen. Ist der Task `Running` oder liegt eine Warnfrist vor, keine unnötige App-Aktualisierung vornehmen. Ist der Lauf beendet oder aus, kann die alte Tray-App sauber beendet und die neue EXE gestartet werden.

Nach dem Neustart prüft die App beim Start `backend::status()`. Fällt dieser Aufruf fehl, zeigt sie zunächst „Aus“ und meldet den Fehler im Menü. Das ist eine Anzeigegrenze, keine Freigabe zum manuellen Shutdown. In diesem Fall zuerst den WSL-Status und das Log prüfen.

## Dokumentation mitpflegen

Bei jeder fachlichen Änderung dieses Systems zuerst prüfen, ob eine Sicherheitsinvariante, ein Menüeintrag, ein Standardwert, ein Pfad oder ein Diagnoseweg betroffen ist. Dann die passende Seite in diesem Ordner und [CHANGELOG.md](./CHANGELOG.md) aktualisieren. Die Dokumentation soll nicht das Marketingbild des Tools sein, sondern einem späteren Bearbeiter zuverlässig sagen, welche Änderung sicher ist und welche nicht.
