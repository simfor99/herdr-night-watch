# Dauerhafter Statuskanal

Der Windows-Tray und das Live-Fenster fragen den Anzeigestatus über jeweils einen dauerhaften WSL-Helfer ab. Wiederholte Refreshes verwenden dessen stdin/stdout-Verbindung. Der manuelle Befehl `--status` bleibt verfügbar.

Die Implementierung liegt in `src/status_channel.rs`, `src/backend.rs` und `watcher/status_channel.py`. EXE, Watcherskript und das daneben liegende neue Python-Modul müssen gemeinsam aktualisiert werden. Der Kanalmodus heißt `--status-channel`; er erhält die native Windows-Startzeit vom aufrufenden Windows-Prozess. Er ist kein manuell zu startender Nachtlauf.

Die Anzeige ermittelt die Windows-Startzeit ohne PowerShell-Prozess über `NtQuerySystemInformation`. Der FILETIME-Wert wird in das bestehende siebenstellige .NET-Zeitformat umgewandelt. Die vorhandene Statuspflege einschließlich Neustarterkennung und kurzlebiger Abschlusssperre bleibt erhalten. Einmalige CLI- und Energieaktionen verwenden weiterhin ihre bisherigen Prüfungen.

Das versionierte JSON-Zeilenprotokoll prüft Anfragekennung, Sequenz und Erzeugungszeit. stdout enthält ausschließlich Protokollnachrichten; Diagnoseausgaben gehen an stderr und werden mit festem Lesepuffer abgeführt. Antworten und Eingaben sind auf 256 KiB begrenzt. Eine Statusanfrage erhält im Helfer 25 Sekunden; der Client wartet höchstens 26 Sekunden und räumt anschließend innerhalb von 3,2 Sekunden auf. Statusbezogenes Lock-Warten ist auf zwei Sekunden und ein Herdr-Aufruf auf fünf Sekunden beziehungsweise das verbleibende Anfragebudget begrenzt.

Beim Schließen fällt stdin weg. Der Helfer unterbricht eine laufende Anfrage, beendet die eigenen Herdr-Prozessgruppen und bestätigt erst danach das Aufräumen. Ohne bestätigten Abschluss startet der Client keine weitere Helfergeneration, pausiert aber nur 60 Sekunden und versucht danach eine neue Generation, damit ein WSL-Neustart unter der laufenden App die Verbindung nicht dauerhaft trennt. Nach bestätigtem Aufräumen verzögern wiederholte Fehler weitere Versuche über exponentielles Backoff bis 64 Sekunden, stoppen sie aber nie dauerhaft; ein erfolgreicher Abruf setzt das Fehlerbudget zurück. Die Neustarterkennung vergleicht die Windows-Startzeitmarker von `arm()` (CIM) und Statuskanal (native Ticks) mit Sekundentoleranz, damit derselbe Systemstart nicht fälschlich als Neustart einen laufenden Nachtlauf zurücksetzt. Es gibt keinen Rückfall auf wiederholte `--status`-Prozesse.

Ein fehlender Status erscheint ausdrücklich als unbekannt; vorherige Daten werden als veraltet bezeichnet. Startaktionen bleiben dann gesperrt, Stopp bleibt verfügbar. Der Anzeigestatus löst selbst keine neue Energieaktion aus.

## Prüfung am 12. Oktober 2026

73 Python-Tests und 39 unter Linux ausführbare Rust-Tests bestanden; Windows-Release wurde gebaut. Der unabhängige Team-Red-Review fand nach Korrekturen keine weiteren konkreten Blocker. Ein nativer Windows-Test mit dem Produktions-Transportmodul lieferte 24 Antworten über zwei dauerhafte WSL-Verbindungen mit isoliertem Zustand und künstlicher Herdr-CLI. Dabei entstanden zwei initiale unsichtbare Konsolen, keine weiteren durch Refreshes. Große stderr-Ausgaben blockierten den Kanal nicht. Ein separater Abbruchtest entfernte den Helfer und die hängende künstliche CLI samt Kind.

Die Tests starteten keinen echten Nachtlauf und keine Energieaktion. Ein Neustart des gesamten Rechners und die vollständige installierte Oberfläche wurden damit nicht geprüft. Das ist ein Nachweis für den Statuskanal, keine Zusicherung, dass sämtliche Fenster anderer Programme verschwinden.

Testbefehle: `python3 -m unittest discover -s watcher -p 'test*.py' -q`, `cargo test`, `cargo build --release --target x86_64-pc-windows-gnu`. TypeScript ist in diesem Rust/Python-Projekt nicht eingerichtet; der angeforderte `npx --no-install tsc --noEmit` konnte deshalb keinen TypeScript-Check ausführen.
