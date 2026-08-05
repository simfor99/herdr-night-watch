> [Zurück zum Index](./INDEX.md) | [Systembild](01-systembild.md) | [Bedienung und Zustände](02-bedienung-und-zustaende.md) | **Betrieb und Fehlerdiagnose** | [Entwicklung und Test](04-entwicklung-und-test.md)

# Betrieb und Fehlerdiagnose

> **Zweck:** Probleme nachvollziehen, ohne den Sicherheitsvertrag durch Schnellreparaturen zu schwächen.
> **Quellstand:** 2026-08-04

---

## Erst prüfen, dann eingreifen

Bei einem auffälligen Nachtlauf ist der Status der beste Einstieg. Ein voreiliges Löschen der Zustandsdateien kann verschleiern, ob ein Shutdown noch abgebrochen werden muss. Der offizielle Stopp-Pfad räumt beides geordnet auf: den Wächter und einen von ihm selbst angelegten ausstehenden Shutdown.

Die Bedienoberfläche ist absichtlich nicht der einzige Beobachtungspunkt. Der Windows-Task zeigt, ob der Hintergrundprozess läuft, und der WSL-Status zeigt, dass die aktuelle Herdr-Arbeit überwacht wird. Das Protokoll erklärt danach die konkrete Entscheidung in zeitlicher Reihenfolge.

Zusätzlich schreibt der Wächter `diagnostics.jsonl` neben `watch.log`. Jede Zeile ist ein einzelnes, maschinenlesbares JSON-Ereignis mit UTC-Zeit, Prozess-ID, Ereignis und Detail. Es bleiben die letzten 500 Ereignisse erhalten. Diese Datei ist der erste Anhaltspunkt, wenn ein Lauf hängen blieb, unerwartet endete oder keine Warnung auslöste; sie lässt sich von Codex direkt auswerten.

## Normale Betriebsprüfung

Die folgenden Befehle werden in einer Windows-PowerShell ausgeführt. Sie verändern nichts.

```powershell
& "<Repository>\windows\Get-HerdrNightWatchStatus.ps1"
```

Erwartung: Der Task ist für einen aktiven Lauf `Running`; der Wächter gibt JSON mit `state` aus. Ohne aktiven Lauf lautet die WSL-Antwort `No night watch is armed.`. Nach einem regulären Ende kann `active-run.json` noch den letzten Lauf mit `outcome` enthalten; das ist kein aktiver Wächter.

Das Tray-Menü „Protokoll öffnen“ öffnet dieselbe Quelle. Alternativ liegt sie im WSL-Zustandsordner unter `~/.local/state/herdr-night-watch/watch.log` und `diagnostics.jsonl`.

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
| Kein Popup im aktiven Lauf. | Tray-App ist beendet oder erkennt die Warnphase nicht; der Wächter läuft trotzdem weiter. | Task- und WSL-Status prüfen, bei Bedarf mit dem Stopp-Pfad abbrechen. |
| Warnung nennt fehlendes Internet. | Beide Verbindungschecks waren fünf Minuten nicht erreichbar. | Internet wiederherstellen: Die eigene Warnung wird abgebrochen. Andernfalls Countdown bewusst bestätigen oder abbrechen. |
| Kurz sichtbare Terminalfenster. | Task oder Hilfsprozess wurde sichtbar statt verborgen gestartet. | Task-Aktion prüfen und `Install-HerdrNightWatch.ps1` erneut ausführen. |
| Neuer Lauf meldet, ein anderer Wächter laufe noch. | Ein vorheriger Task gibt seine Dateisperre gerade frei. | Der neue Wächter wartet bis zu 15 Sekunden auf die Freigabe; danach nur bei weiter bestehender Sperre Diagnose beginnen. |
| Das Symbol ist nicht zu sehen. | Windows hat es in den ausgeklappten Taskleistenbereich verschoben. | Pfeil `^` in der Taskleiste öffnen; Prozess erst danach prüfen. |

## Kein sichtbares WSL-Fenster

Der Task muss `wscript.exe` mit [`Run-HerdrNightWatchHidden.vbs`](../windows/Run-HerdrNightWatchHidden.vbs) starten. Die VBS-Datei ruft `wsl.exe ... --watch` über `WScript.Shell.Run` mit Fensterstil `0` auf. In der Tray-App setzen [`backend::wsl()`](../src/backend.rs), PowerShell-Aufrufe und der Registry-Zugriff zusätzlich `CREATE_NO_WINDOW`.

Wenn wieder ein Fenster aufblitzt, nicht am Wächter eine Endlosschleife vermuten. Zuerst in der Aufgabenplanung die Aktion des Tasks `Herdr Night Watch` kontrollieren. Sie muss auf `C:\WINDOWS\System32\wscript.exe` zeigen und als Argument den UNC-Pfad zur VBS-Datei erhalten. Danach den Installationsbefehl erneut ausführen:

```powershell
& "<Repository>\windows\Install-HerdrNightWatch.ps1"
```

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

Wenn der Task fehlt oder nach einer Windows-/WSL-Änderung nicht mehr startet, zuerst verifizieren, dass die WSL-Distribution `Ubuntu` erreichbar ist und dort Herdr verfügbar ist. Danach `Install-HerdrNightWatch.ps1` ausführen. Dieses Skript prüft `wsl.exe`, ruft die Hilfe des Wächters auf und registriert den Task neu.

Die Tray-App selbst wird unabhängig gestartet. Ihr Autostart wird über den Menüpunkt „Mit Windows starten“ im Benutzerzweig der Registry verwaltet. Das Wiederinstallieren des Tasks darf nicht dazu führen, dass ein Nachtlauf automatisch beim Windows-Login beginnt.
