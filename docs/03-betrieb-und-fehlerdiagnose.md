> [Zurück zum Index](./INDEX.md) | [Systembild](01-systembild.md) | [Bedienung und Zustände](02-bedienung-und-zustaende.md) | **Betrieb und Fehlerdiagnose** | [Entwicklung und Test](04-entwicklung-und-test.md)

# Betrieb und Fehlerdiagnose

> **Zweck:** Probleme nachvollziehen, ohne den Sicherheitsvertrag durch Schnellreparaturen zu schwächen.
> **Quellstand:** 2026-08-04

---

## Erst prüfen, dann eingreifen

Bei einem auffälligen Nachtlauf ist der Status der beste Einstieg. Ein voreiliges Löschen der Zustandsdateien kann verschleiern, ob ein Shutdown noch abgebrochen werden muss. Der offizielle Stopp-Pfad räumt beides geordnet auf: den Wächter und einen von ihm selbst angelegten ausstehenden Shutdown.

Die Bedienoberfläche ist absichtlich nicht der einzige Beobachtungspunkt. Der WSL-Status zeigt, dass die aktuelle Herdr-Arbeit überwacht wird. Das Protokoll erklärt danach die konkrete Entscheidung in zeitlicher Reihenfolge.

Zusätzlich schreibt der Wächter `diagnostics.jsonl` neben `watch.log`. Jede Zeile ist ein einzelnes, maschinenlesbares JSON-Ereignis mit UTC-Zeit, Prozess-ID, Ereignis und Detail. Es bleiben die letzten 500 Ereignisse erhalten. Diese Datei ist der erste Anhaltspunkt, wenn ein Lauf hängen blieb, unerwartet endete oder keine Warnung auslöste; sie lässt sich von Codex direkt auswerten.

Nach einem Windows- oder WSL-Neustart wird ein unvollständig liegen gebliebener Lauf nicht fortgesetzt. Der Wächter erkennt die geänderte WSL- oder Windows-Boot-Marke, schreibt ein `RESET`-Ereignis mit `reason=boot_id_changed` oder `reason=missing_boot_id_marker` und entfernt die alte Warnungsdatei. Im `cancellation-history.csv` steht zusätzlich der Auslöser `system_restart`.

Wenn Windows trotz aktivem Nachtlauf schlafen möchte, prüfe zuerst, ob die Tray-App noch läuft und den Status aktuell anzeigt. Nur ein aktiver Zustand `Watching` oder `ShutdownWarning` hält den Leerlauf-Schutz. Der Schutz verhindert keinen manuellen Schlaf- oder Shutdown-Befehl und wird absichtlich aufgehoben, sobald der Nachtlauf endet.

Kann Windows die Energiesperre nicht setzen, stoppt die Tray-App den gerade gestarteten Nachtlauf sicher und zeigt den Fehler weiter an. Ein vorübergehend fehlender Windows-Bootmarker führt dagegen nicht zu einem Teilvergleich: Der Reset wird erst bewertet, wenn WSL- und Windows-Marke gemeinsam vorliegen.

## Normale Betriebsprüfung

Die folgenden Befehle werden in einer Windows-PowerShell ausgeführt. Sie verändern nichts.

```powershell
& "<Repository>\windows\Get-HerdrNightWatchStatus.ps1"
```

Erwartung: Der Wächter gibt JSON mit `state` aus. Ohne aktiven Lauf lautet die WSL-Antwort `No night watch is armed.`. Nach einem regulären Ende kann `active-run.json` noch den letzten Lauf mit `outcome` enthalten; das ist kein aktiver Wächter.

Das Tray-Menü „Protokoll öffnen“ öffnet dieselbe Quelle. Alternativ liegt sie im WSL-Zustandsordner unter `~/.local/state/herdr-night-watch/watch.log` und `diagnostics.jsonl`.

Ein Eintrag „Tray-App unplanmäßig beendet“ wird beim nächsten Start erzeugt, wenn die vorherige Tray-Sitzung keinen sauberen Abschlussmarker hinterlassen hat. Das ist ein belastbarer Hinweis auf einen unerwarteten Prozess- oder Systemabbruch, aber kein Beweis für einen spezifischen Absturzgrund. Eine erwartete Energieaktion wird nicht als Absturz protokolliert.

## Sicher stoppen

Wenn ein Lauf nicht weiterlaufen soll, immer den vorgesehenen Stopp verwenden:

```powershell
& "<Repository>\windows\Stop-HerdrNightWatch.ps1"
```

Das Skript ruft zuerst `herdr-night-watch.py --cancel` auf und stoppt danach den Windows-Task, sofern er noch läuft. `--cancel` versucht `shutdown.exe /a` nur, wenn eine gültige eigene `shutdown-warning.json` existiert. Deshalb darf dieser Befehl nicht durch einen pauschalen `shutdown /a`-Alias ersetzt werden: Der Wächter soll keine fremden Shutdowns beeinflussen.

## Häufige Bilder und ihre Ursache

| Beobachtung | Wahrscheinliche Ursache | Sicherer nächster Schritt |
|---|---|---|
| Start meldet „No working Herdr agents found“. | Beim Scharfstellen war kein Herdr-Agent im Status `working`. | Arbeit starten oder nur die Demo verwenden. |
| Lauf endet mit `refused`. | Ein Agent ist `blocked` oder `unknown`, ein Status fehlt oder Herdr konnte nicht eindeutig prüfen. | `watch.log` lesen; nicht durch einen manuellen Shutdown ersetzen. |
| Gelb dauert länger als erwartet. | Ruhezeit wird bei erneut aktiver Herdr-Arbeit zurückgesetzt; Standard sind 5 Sekunden. | Status und Log prüfen; bei Bedarf Lauf stoppen und mit bewusst anderer Ruhezeit starten. |
| Kein Popup im aktiven Lauf. | Tray-App ist beendet oder erkennt die Warnphase nicht; der Wächter läuft trotzdem weiter. | WSL-Status prüfen, bei Bedarf mit dem Stopp-Pfad abbrechen. |
| Warnung nennt fehlendes Internet. | Beide Verbindungschecks waren fünf Minuten nicht erreichbar. | Internet wiederherstellen: Die eigene Warnung wird abgebrochen. Andernfalls Countdown bewusst bestätigen oder abbrechen. |
| Kurz sichtbare Terminalfenster. | Eine alte EXE verwendet noch den früheren Skriptstarter. | Aktuelle EXE installieren und die Tray-App neu starten. |
| Neuer Lauf meldet, ein anderer Wächter laufe noch. | Ein vorheriger Task gibt seine Dateisperre gerade frei. | Der neue Wächter wartet bis zu 15 Sekunden auf die Freigabe; danach nur bei weiter bestehender Sperre Diagnose beginnen. |
| Das Symbol ist nicht zu sehen. | Windows hat es in den ausgeklappten Taskleistenbereich verschoben. | Pfeil `^` in der Taskleiste öffnen; Prozess erst danach prüfen. |

## Kein sichtbares WSL-Fenster

Die Tray-App startet `wsl.exe --watch` direkt mit dem Windows-Flag `CREATE_NO_WINDOW`. Der frühere Windows-Task gehört nicht mehr zum Startpfad eines Nachtlaufs. Wenn ein sichtbares Terminal erscheint, läuft noch eine ältere EXE. Die Tray-App vollständig beenden, die aktuelle EXE installieren und erneut starten.

## Protokoll lesen

Das Log verwendet UTC-Zeitstempel. Nützliche Marker sind `ARMED`, `TARGET`, `WATCHING`, `STATUS`, `FINISHED` und `ERROR`. Ein guter Diagnoseausschnitt besteht aus dem letzten `ARMED` bis zum zugehörigen `FINISHED`, nicht nur aus der letzten einzelnen Zeile.

Beim raschen Stoppen und erneuten Starten kann die alte WSL-Instanz noch wenige Augenblicke ihren Lock halten. `watch()` wartet deshalb bis zu 15 Sekunden auf diese Freigabe, statt den neuen Lauf sofort mit einem Sperrfehler enden zu lassen. Das ist kein zweiter paralleler Wächter: Erst nach erfolgreicher Sperre beginnt der neue Prozess seine Überwachung.

| Marker | Bedeutung |
|---|---|
| `ARMED` | Live-Prüfbereich aktiviert, inklusive Zahl arbeitender Agenten beim Start |
| `STATUS` | Aktueller Herdr-Status aller gemeldeten Agenten |
| `No Herdr agents are working` | Ruhezeit hat begonnen |
| `DRY RUN` | Demo oder Beobachtungsmodus - kein echter Shutdown |
| `Windows shutdown scheduled` | Echter Shutdown wurde mit Warnfrist angesetzt |
| `FINISHED outcome=refused` | Fail-closed: keine eindeutige Grundlage für Shutdown |
| `Shutdown warning cancelled` | Neue Herdr-Arbeit hat den Countdown abgebrochen; der Nachtlauf beobachtet weiter |
| `Internet connection unavailable` | Fünfminütige Toleranzzeit für einen Verbindungs-Ausfall hat begonnen |
| `COMPLETION HISTORY` | Abschluss wurde in `logs\\completion-history.csv` eingetragen |
| `CANCELLATION HISTORY` | Abbruchquelle wurde in `logs\\cancellation-history.csv` eingetragen |

## Wiederherstellung nach einer Installation

Wenn die Tray-App Herdr nicht erreicht, zuerst im Setup-Fenster Distro und Wächterpfad prüfen. Danach verifizieren, dass die WSL-Distribution erreichbar ist und dort Herdr verfügbar ist. Die Tray-App selbst kann über den Menüpunkt „Mit Windows starten“ beim Benutzer-Login gestartet werden; ein Nachtlauf wird dadurch niemals automatisch aktiviert.
