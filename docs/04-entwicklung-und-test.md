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
- `completion-history.csv` protokolliert die angeforderten Energieaktionen des WSL-Wächters. Der Datensatz wird vor einer bestätigten Schlaf- oder Shutdown-Aktion dauerhaft geschrieben; bei einem Schreibfehler wird keine Energieaktion angefordert. `tray-history.csv` protokolliert erkannte unplanmäßige Tray-Enden. Der Log-Viewer zeigt beide Dateien in getrennten Bereichen und höchstens 30 Einträge je Bereich.
- `cancellation-history.csv` darf höchstens 30 Ereignisse enthalten und muss die konkrete Abbruchquelle speichern.
- `--cancel` darf nur die eigene Warnung entfernen und niemals einen fremden Windows-Shutdown mit `shutdown.exe /a` beeinflussen.
- Während der Warnfrist darf noch kein Windows-Shutdown laufen. Erst die letzte erfolgreiche Sicherheitsprüfung schreibt Verlauf und Endzustand und fordert danach Windows zum sofortigen Herunterfahren auf.
- Lässt sich die eigene Warnung vor einer notwendigen Stornierung oder Energieaktion nicht sicher lesen und entfernen, endet der Lauf mit `shutdown_abort_failed`; Verlauf und Windows-Energieaktion werden dann nicht fortgesetzt.
- Die Tray-App und ihre Hilfsprozesse dürfen keine sichtbaren Konsolenfenster erzeugen.
- Solange die Tray-App läuft, muss sie den automatischen Windows-Leerlauf-Energiesparmodus mit `ES_CONTINUOUS | ES_SYSTEM_REQUIRED` verhindern - unabhängig davon, ob ein Nachtlauf aktiv ist. Der Bildschirm und bewusste Energieaktionen bleiben davon unberührt.
- Lehnt Windows diese Energiesperre beim Start ab, darf die primäre Tray-App nicht weiterlaufen. Ein aktiver oder nicht sicher abfragbarer Nachtlauf wird vorher mit `power_guard_failed` beendet.
- `diagnostics.jsonl` muss für jede relevante Zustandsänderung maschinenlesbare Ereignisse liefern und auf die letzten 500 Einträge begrenzt bleiben.
- Eine neue Wächterinstanz darf erst nach Freigabe des Lock des Vorgängers prüfen.
- Der Wächter darf nach einem Neustart erst überwachen, wenn eine vollständige WSL-/Windows-Boot-Marke bestätigt wurde; Reset und Watch müssen dieselbe bestätigte Marke verwenden.
- Tooltip und Live-Fenster zeigen die aktuelle Herdr-Sicht; die Shutdown-Prüfung liest diesen Status unabhängig erneut.
- Die Systemtelemetrie im Live-Fenster ist reine Anzeige. Fehler oder fehlende Werte dürfen den Wächter und seine Energieentscheidung nicht beeinflussen.
- Das visuelle Fenster-Chrome liegt zentral in `src/window_chrome.rs`; insbesondere darf die Transparenzsuche nie ein gleichnamiges Fenster eines anderen Prozesses verändern.
- `WS_EX_LAYERED` darf nur bei einer echten Transparenz unter 100 Prozent gesetzt werden. Bei voller Deckkraft muss der Layered-Stil entfernt bleiben, sonst bleibt das Glow-Fenster weiß.
- Der Tray darf ein 4x4-Vorbereitungsfenster nie mit `ShowWindow` sichtbar machen.
- Das monitorgroße, transparente `tray_icon_app`-Hilfsfenster des Trays zählt niemals als Live-Fenster. Ein Klick auf das Mondsymbol muss das echte `--live-status`-Fenster finden oder neu starten.
- Ein bereits vorhandenes Live-Fenster darf auch dann eingeblendet werden, wenn es nach einem Neustart noch versteckt oder noch ohne Titel ist; Hilfsfenster bleiben ausgeschlossen.
- Endet die Tray-App, muss das von ihr gestartete Live-Fenster mitgehen. Das Fenster darf nicht als Waisenprozess weiterlaufen.
- Ein Doppelklick auf die EXE startet den Tray und öffnet das Live-Fenster. Eine zweite EXE-Instanz startet keinen zweiten Tray, sondern holt das vorhandene Live-Fenster.

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

Der Tray aktiviert für seine gesamte Lebensdauer einen Windows-Energieschutz gegen automatischen Leerlauf-Schlaf. Der manuelle Smoke-Test prüft den echten Windows-Pfad: Er setzt den Schutz für den aktuellen PowerShell-Prozess, sucht ihn mit `powercfg.exe /requests` in der `SYSTEM`-Sektion und gibt ihn anschließend wieder frei. Der Test fordert weder Energiesparmodus noch Herunterfahren an.

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
| Live-Status | Mehrere Herdr-Agenten laufen lassen, Live-Status öffnen | Live-Zahl, großer Mond mit echter Mondphase und Hover-Hinweis sowie funktionierender Start- oder Stoppknopf sind sichtbar; grüne Stunden- und Minutenzeiger sowie ein heller blass-oranger Sekundenzeiger zeigen die lokale Zeit, bei aktivem grünen Nachtmodus werden Stunden- und Minutenzeiger warmgelb; kräftige Russo-One-Stundenziffern mit dunkler Kontur und Zwischenstriche liegen im erweiterten Mondhof; die Temperatur sitzt mittig im freien Mondhof oberhalb der Uhr und bleibt bei jeder Mondphase lesbar; Abschluss-Schalter und Sekundenfeld liegen rechts neben „Herdr jetzt“, speichern nur außerhalb eines aktiven Laufes; bei Warnfrist zeigt das Fenster einen roten Countdown; die schmale Fußzeile zeigt CPU, GPU, belegten VRAM, RAM und einen verfügbaren Grafikkarten-Wattwert oder `—`; Fenster ist beweglich und schließbar |
| Live-Status vom Tray | App nur als Tray starten, danach Einfach- und Doppelklick auf den Mond | Es erscheint das echte Live-Fenster mit Titel; das transparente Tray-Hilfsfenster bleibt unsichtbar und wird nicht eingeblendet |
| Tray beenden | Live-Fenster öffnen, danach die Tray-App beenden | Das Live-Fenster schließt sich mit; kein `--live-status`-Prozess bleibt zurück |
| Live-Status nach Neustart | Tray-App mit aktivierter Startoption nach Windows-Login starten; anschließend ein verstecktes Live-Fenster erneut öffnen | Ein vorhandenes Fenster wird restauriert und nicht durch eine zweite Mutex-Instanz ersetzt |
| EXE ohne laufenden Tray | Installierte EXE per Doppelklick starten | Tray erscheint und das Live-Fenster öffnet sich sichtbar |
| EXE bei laufendem Tray | EXE erneut doppelklicken | Es entsteht kein zweiter Tray; das vorhandene Live-Fenster kommt nach vorn |
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
| Proportionale Live-Skalierung | Den Anfasser unten rechts ziehen, Vorschau prüfen, loslassen, danach neu öffnen und per Rechtsklick zurücksetzen | Während des Ziehens bleibt das Layout stabil; beim Verkleinern bleiben Prozent- und Pixelangabe im aktuellen Fenster sichtbar; beim Loslassen wächst oder schrumpft das komplette Layout ohne Verzerrung, die Größe bleibt gespeichert und `Auf 100 % zurücksetzen` stellt den Ausgangswert wieder her |
| Analoge Monduhr | Live-Status öffnen, Uhrzeit mit Windows vergleichen, Ziffern, Randstriche und Zeigerspitzen ansehen, Nachtmodus starten und stoppen, direkt auf den Mond rechtsklicken und `Orangen Sekundenzeiger anzeigen` sowie `Analoguhr anzeigen` jeweils zweimal umschalten | Stunde und Minute stimmen mit Windows überein; `12`, `3`, `6` und `9` ersetzen die vier breiten Marken, acht schmale Zwischenstriche bleiben und alles schwebt vollständig außerhalb der Mondscheibe im erweiterten Hof; Russo One zeichnet die 50 Prozent größeren, hellen Ziffern mit dunkler Kontur und bleibt auch im kleinen Fenster scharf; Stunden- und Minutenzeiger sind außerhalb des grünen Nachtmodus grün und wechseln dort auf warmes Gelb, behalten dabei weiße und schwarze Doppelkontur und die klare Instrumentenspitze; der lange, schmale und blass-orange Sekundenzeiger reicht knapp über den Mondrand, verschwindet und erscheint sofort; bei ausgeschalteter Analoguhr verschwinden Zifferblatt und Zeiger, die Temperatur bleibt mittig im Mond und das Wetterzeichen ist ausgeblendet; jede Auswahl schließt das Menü sofort und die letzte Auswahl bleibt nach erneutem Öffnen erhalten |
| Windows-Energieschutz | Tray ohne Nachtmodus starten, `powercfg /requests` prüfen, Tray beenden; zusätzlich als Administrator `windows\Test-HerdrNightWatchPowerGuard.ps1` ausführen | Der Tray erscheint auch bei ausgeschaltetem Nachtmodus in der `SYSTEM`-Sektion; nach dem Beenden verschwindet sein Request und der Testprozess gibt seinen eigenen Request ebenfalls frei |
| Tray-Absturz-Erkennung | Tray starten, Prozess während eines inaktiven Zustands gezielt beenden, Tray neu starten | `logs\\tray-history.csv` enthält genau einen Hinweis „Tray-App unplanmäßig beendet“; ein erwarteter Energiesparmodus erzeugt keinen solchen Hinweis |

## Prüfen vor dem Neustart der Tray-App

Vor einem Neustart ist der Status wichtig. In Windows zuerst `Get-HerdrNightWatchStatus.ps1` ausführen. Ist der Task `Running` oder liegt eine Warnfrist vor, keine unnötige App-Aktualisierung vornehmen. Ist der Lauf beendet oder aus, kann die alte Tray-App sauber beendet und die neue EXE gestartet werden.

Nach dem Neustart prüft die App beim Start `backend::status()`. Fällt dieser Aufruf fehl, zeigt sie zunächst „Aus“ und meldet den Fehler im Menü. Das ist eine Anzeigegrenze, keine Freigabe zum manuellen Shutdown. In diesem Fall zuerst den WSL-Status und das Log prüfen.

## Dokumentation mitpflegen

Bei jeder fachlichen Änderung dieses Systems zuerst prüfen, ob eine Sicherheitsinvariante, ein Menüeintrag, ein Standardwert, ein Pfad oder ein Diagnoseweg betroffen ist. Dann die passende Seite in diesem Ordner und [CHANGELOG.md](./CHANGELOG.md) aktualisieren. Die Dokumentation soll nicht das Marketingbild des Tools sein, sondern einem späteren Bearbeiter zuverlässig sagen, welche Änderung sicher ist und welche nicht.
