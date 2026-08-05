# Herdr-Nachtwächter

Kleine Windows-Tray-App für den vorhandenen fail-closed Herdr-Nachtwächter.

Die App steuert den robusten WSL-Hintergrundwächter, ersetzt ihn aber nicht.

Dieses Repository enthält die Rust-Tray-App, den Python-Wächter, Windows-Skripte, Tests, Dokumentation und eine getestete Windows-EXE.

## Warum dieses Tool existiert

Wir lassen Herdr häufig über Nacht laufen, weil Compound-Engineering-Pläne sehr groß werden können. Sobald ein solcher Plan ausgeführt wird, arbeiten mehrere Agenten oft stundenlang selbstständig weiter. Der Rechner muss dafür aber nicht die ganze Nacht laufen: Wenn alle Arbeiten beendet sind, soll Windows zuverlässig in den Energiesparmodus wechseln oder herunterfahren.

Genau diese Übergabe zwischen „Herdr ist fertig“ und „Windows darf schlafen gehen“ war vorher nicht zuverlässig gelöst. Der Herdr-Nachtwächter schließt diese Lücke: Er überwacht die Agenten fail-closed, zeigt den Zustand sichtbar an und führt die gewählte Energieaktion erst nach einer konfigurierbaren Warnfrist aus.

Die Oberfläche unterstützt Deutsch und English. Die Sprache lässt sich im Tray-Rechtsklickmenü unter „Sprache / Language“ umstellen.

## Live-Status

![Herdr-Nachtwächter Live-Status](docs/images/live-status-de.png)

## Bedienung

- **Nachtmodus starten**: beobachtet fortlaufend alle aktuell von Herdr gemeldeten Agenten und fährt Windows erst nach der konfigurierten Ruhezeit herunter.
- **Nur beobachten**: gleicher Lauf, aber ohne Shutdown.
- **Stopp und Shutdown abbrechen**: beendet den Lauf und bricht ausschließlich einen vom Wächter ausgelösten, noch ausstehenden Shutdown ab.
- **Demo: Abschluss simulieren**: zeigt in wenigen Sekunden die Ruhezeit und die Shutdown-Warnung. Sie kann Windows niemals herunterfahren.
- **Live-Status öffnen**: öffnet ein frei platzierbares, jederzeit schließbares Statusfenster. Linksklick auf das Tray-Symbol öffnet es, Rechtsklick zeigt das Menü.
- **Mit Windows starten**: startet nur die Tray-App beim Windows-Login. Sie scharfstellt keinen Nachtlauf automatisch.

Der Python-Wächter liegt unter `watcher/herdr-night-watch.py`; sein Sicherheitsvertrag bleibt maßgeblich.

## Installation

1. Stelle WSL mit einer installierten Herdr-CLI und einer Ubuntu-Distro bereit.
2. Passe in den Windows-Skripten `-Distro` und `-CodexHome` an deine WSL-Installation an. `CodexHome` ist der WSL-Pfad, unter dem `watcher/herdr-night-watch.py` liegt.
3. Führe `windows/Install-HerdrNightWatch.ps1` in PowerShell aus.
4. Starte `dist/Herdr-Nachtwaechter.exe`.

Die EXE ist ein Komfortartefakt für Windows. Für andere Architekturen oder nach Codeänderungen kann sie aus `src/` neu gebaut werden.

Der Tooltip und das Statusfenster zeigen die aktuelle Herdr-Zahl nur als Anzeige. Kann Herdr gerade nicht gelesen werden, wird keine Zahl geraten und die Shutdown-Entscheidung bleibt unverändert fail-closed.

Im echten Nachtmodus beginnt nach fünf Sekunden bestätigter Inaktivität die 300-Sekunden-Warnfrist. Der Windows-Dialog bietet `Abbrechen`; damit stoppt er den Wächter und hebt genau diesen geplanten Shutdown wieder auf.

## Wartungsdokumentation

Die vollständige technische Dokumentation für spätere Änderungen beginnt in [docs/INDEX.md](docs/INDEX.md). Sie beschreibt die Zustände, die Trennung zwischen Windows und WSL, Build und Auslieferung sowie die sichere Fehlersuche. Laufzeitdaten, Logs und persönliche Zustandsdateien gehören nicht in Git.

## Icon

Das Tray-Icon basiert auf Googles offiziellem Material-Icon `bedtime` und ist unter Apache-2.0 lizenziert. Je nach Zustand wird es grau, grün, blau, gelb oder rot eingefärbt.
