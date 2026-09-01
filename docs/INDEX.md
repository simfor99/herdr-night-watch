# Herdr-Nachtwächter - Wartungsdokumentation

> **Produkt:** Herdr-Nachtwächter
> **Status:** In Betrieb, Quellstand vom 2026-08-04
> **Erstellt:** 2026-08-04
> **Letztes Update:** 2026-08-04

---

## Warum dieses System?

Wenn mehrere Herdr-Panes über Nacht offen bleiben, ist ein gewöhnlicher Windows-Shutdown zu grob: Er kann noch arbeitende Agenten unterbrechen. Umgekehrt ist es frustrierend, wenn ein eigentlich fertiger Rechner bis zum Morgen weiterläuft, weil niemand zuverlässig auf den Abschluss schaut. Entscheidend ist dabei nicht, ob irgendein Terminal offen ist, sondern ob Herdr aktuell noch Arbeit meldet.

Der Herdr-Nachtwächter löst das mit einem absichtlich vorsichtigen Prinzip. Nach dem Start prüft er fortlaufend alle aktuell von Herdr gemeldeten Agenten. Erst wenn keiner mehr arbeitet und dieser Zustand über die Ruhezeit stabil bleibt, wird ein Shutdown vorbereitet. Bei Unklarheit - etwa bei `blocked`, `unknown`, fehlendem Status oder keiner verlässlichen Herdr-Antwort - wird nicht heruntergefahren. Das ist der „fail-closed“-Vertrag: Im Zweifel bleibt Windows an.

Dadurch wird aus einem manuellen Nacht-Ritual eine überprüfbare Automatik. Die Tray-App macht den Zustand sichtbar und bietet Start, Stopp, Beobachtung und eine gefahrlose Demo. Der Hintergrundwächter bleibt davon getrennt, damit ein versehentlich geschlossenes Tray-Symbol keinen bereits kontrolliert laufenden Wächter beendet.

| Vorher | Nachher |
|---|---|
| Offene Terminals waren kein brauchbares Signal für fertige Arbeit. | Alle aktuell von Herdr gemeldeten Agenten bestimmen den Nachtlauf. |
| Ein Shutdown wäre bei einer Unsicherheit riskant gewesen. | Jede Unsicherheit verweigert den Shutdown. |
| Prüfung und Bedienung erforderten sichtbare Konsolenfenster. | Die Tray-App startet den WSL-Wächter als unsichtbaren Hintergrundprozess. |

## Was wir gebaut haben

Beim Klick auf „Nachtmodus starten“ erfasst die Windows-Tray-App über WSL die aktive Herdr-Arbeit und startet danach `herdr-night-watch.py --watch` direkt als fensterlosen Hintergrundprozess. Dafür ruft sie `wsl.exe` mit `CREATE_NO_WINDOW` auf. Der Wächter prüft Herdr in festen Abständen, führt die Ruhezeit und danach eine widerrufbare Warnfrist aus. Erst nach deren Ablauf und der letzten Sicherheitsprüfung fordert er Windows zum Herunterfahren auf.

Währenddessen fragt die Tray-App den WSL-Zustand etwa alle fünf Sekunden ab und färbt ihr Mond-Symbol passend ein. In der echten Warnphase erscheint ein Windows-Dialog: „Abbrechen“ ruft den Stopp-Pfad auf und entfernt die eigene Warnung. Windows wird erst nach Ablauf und letzter Sicherheitsprüfung zum Herunterfahren aufgefordert. Die Demo folgt derselben sichtbaren Zustandsfolge, kann aber technisch niemals `shutdown.exe /s` ausführen.

| Kernpunkt | Wert |
|---|---|
| Standard-Ruhezeit | 5 Sekunden |
| Standard-Prüfintervall | 1 Sekunde |
| Echte Warnfrist | 300 Sekunden |
| Demo-Ruhezeit und -Warnfrist | 8 Sekunden und 15 Sekunden |
| Hintergrundstart | `wsl.exe` mit `CREATE_NO_WINDOW` und `herdr-night-watch.py --watch` |
| Sicherheitsprinzip | Bei Zweifel kein Shutdown |

## Wie es zusammenhängt

Dieses Projekt verbindet sich mit [Herdr](../../../.agents/skills/herdr/SKILL.md), weil der Wächter dessen Agentenstatus und Pane-Prozessdaten als Grundlage für seine Entscheidung verwendet. Ohne einen laufenden, kompatiblen Herdr-Server wird deshalb kein Nachtlauf scharfgestellt. Die Verbindung ist bewusst eng: Shell-Prozesse oder nicht als Herdr-Agent erfasste Arbeit werden nicht geraten oder interpretiert.

Es verbindet sich außerdem direkt mit WSL. Windows besitzt die Herunterfahr-Funktion, WSL kennt Herdr und führt den Python-Wächter aus. Die Tray-App startet `wsl.exe` mit `CREATE_NO_WINDOW`, damit kein Konsolenfenster erscheint. Die vorhandenen Task-Scheduler- und VBS-Skripte bleiben als manuelle Betriebswerkzeuge erhalten, gehören aber nicht mehr zum normalen Tray-Startpfad. Die Tray-App ist separat, weil sie den Ablauf erklären und abbrechen soll, nicht aber die Sicherheitsentscheidung tragen darf.

## Schlüsselentscheidungen

### Aktuelle Herdr-Arbeit statt „alle offenen Panes“

**Kontext:** Der Nachtlauf soll neue relevante Herdr-Arbeit berücksichtigen, aber sich nicht von beliebigen offenen Terminals leiten lassen.

**Entscheidung:** Der Wächter fragt bei jeder Prüfung alle aktuell von Herdr gemeldeten Agenten ab. Nur wenn keiner `working` ist, kann die Ruhezeit laufen.

**Konsequenzen:**

- (+) Neue relevante Herdr-Arbeit wird automatisch berücksichtigt.
- (-) Dauerhaft als `working` gemeldete Arbeit hält den Nachtlauf offen.
- (~) Arbeit während der Warnfrist bricht nur den eigenen Countdown ab; danach wird automatisch weiter beobachtet.

### Fail-closed bei Abweichungen

**Kontext:** Ein fehlender oder unklarer Herdr-Status kann bedeuten, dass aktuelle Arbeit nicht eindeutig erkennbar ist.

**Entscheidung:** Der Wächter verweigert den Shutdown bei `blocked`, `unknown`, fehlendem Status oder nicht erreichbarem Herdr.

**Konsequenzen:**

- (+) Die Automatik fährt nicht auf Basis einer Vermutung herunter.
- (-) Manchmal bleibt der Rechner länger an als unbedingt nötig.
- (~) Log und Status sind die erste Anlaufstelle, wenn ein Lauf mit `refused` endet.

### Unsichtbarer WSL-Prozess statt sichtbarem Terminal

**Kontext:** Ein normal gestartetes `wsl.exe` kann ein sichtbares Windows-Terminal erzeugen.

**Entscheidung:** Die Tray-App startet den Python-Wächter direkt über `wsl.exe` und setzt dabei `CREATE_NO_WINDOW`.

**Konsequenzen:**

- (+) Der Hintergrundlauf bleibt im Hintergrund.
- (-) Fehler sind nicht an einer Konsole sichtbar.
- (~) Für Diagnose wird das Protokoll verwendet, nicht ein offenes Terminal.

## Dokument-Map

| # | Dokument | Zeilen | Inhalt |
|---|---|---:|---|
| 01 | [Systembild](01-systembild.md) | 80 | Komponenten, Pfade und Ablauf von Windows bis Herdr |
| 02 | [Bedienung und Zustände](02-bedienung-und-zustaende.md) | 83 | Tray-Menü, Farben, Popup und Zustandsautomat |
| 03 | [Betrieb und Fehlerdiagnose](03-betrieb-und-fehlerdiagnose.md) | 80 | Installation, Status, Protokoll und sichere Fehlerbehebung |
| 04 | [Entwicklung und Test](04-entwicklung-und-test.md) | 79 | Änderungsregeln, Build, Auslieferung und Testmatrix |

## Leseempfehlung

**Zum Wieder-Einstieg:** Dieses Dokument, dann [Systembild](./01-systembild.md).

**Für eine Bedienungsänderung:** [Bedienung und Zustände](./02-bedienung-und-zustaende.md).

**Bei einem Fehler oder sichtbaren Popup:** [Betrieb und Fehlerdiagnose](./03-betrieb-und-fehlerdiagnose.md).

**Vor einer Code- oder Installationsänderung:** [Entwicklung und Test](./04-entwicklung-und-test.md).

## Änderungshistorie

| Datum | Was |
|---|---|
| 2026-08-04 | Initiale Wartungsdokumentation aus dem geprüften Quellstand erstellt. |
