# Changelog - Herdr-Nachtwächter

> Alle wesentlichen Änderungen an dieser Dokumentation.
> Format: Game-Style mit Fokus auf Leser-Wirkung.

## [2026-08-15] - Stabilitätsrelease v0.1.17

### 🔧 Zuverlässigkeit

- **Gemeinsame Fenster-Schicht** - Transparenz, Fensterebenen, Farbverlauf und Glasreflexion liegen jetzt zentral in `src/window_chrome.rs`.
- **Prozesssichere Transparenz** - Die Opazitätsänderung trifft ausschließlich Fenster der eigenen Tray-App und kann keine gleichnamigen Fremdfenster verändern.
- **Regressionen geprüft** - Python-Wächtertests, Rust-Tests, Cross-Compile und Syntaxprüfung laufen erfolgreich; der manuelle Windows-Energieschutztest bleibt ein bewusst erhöhter, nicht-destruktiver Smoke-Test.

## [2026-08-15] - Leerlauf-Schlaf geschützt und Neustartzustand bereinigt

### 🛡️ Sicherheit

- **Windows schläft nicht ungefragt ein** - Während `Watching` oder `ShutdownWarning` setzt die Tray-App eine Windows-Ausführungsanforderung gegen automatischen Leerlauf-Energiesparmodus. Die Sperre endet beim Stoppen, beim Ende des Laufs oder beim Beenden der App; die eigene bestätigte Energieaktion bleibt möglich.
- **Guard-Fehler bleiben sichtbar und fail-safe** - Kann Windows die Energiesperre nicht setzen, wird ein neuer Nachtlauf sofort wieder gestoppt, statt ungeschützt weiterzulaufen. Der Fehler bleibt im Tray sichtbar, bis die Sperre erfolgreich hergestellt ist.
- **Windows-Neustart sicher erkannt** - Der Wächter kombiniert WSL-Boot-ID und Windows-Bootzeit. Ein alter Nachtlauf wird nach einem Rechnerneustart nicht fortgesetzt, sondern als `system_restart` zurückgesetzt.
- **Unvollständige Boot-Abfrage bleibt neutral** - Fehlt die Windows-Bootzeit vorübergehend, wird kein Teilmarker verglichen und kein aktiver Lauf fälschlich zurückgesetzt oder fortgesetzt.
- **Countdown sicher neu gestartet** - Herdr wird während der gesamten Warnfrist und unmittelbar vor der Energieaktion erneut geprüft. Neue Arbeit bricht die Warnung ab, löscht den Ruhezeit-Marker und startet nach bestätigter Ruhezeit wieder mit der vollständigen Warnfrist.
- **Herdr-Fehler brechen Warnungen ab** - Fällt die Statusabfrage während eines bereits geplanten Countdowns aus, wird die eigene Windows-Aktion abgebrochen und der Wächter kehrt in die Überwachung zurück.
- **Regressionstest ergänzt** - Die Python-Tests prüfen, dass beide Boot-Marken im Neustart-Token enthalten sind und der Resetpfad erhalten bleibt.
- **Windows-Smoke-Test ergänzt** - Ein manueller, nicht-destruktiver Test prüft Aktivierung und Freigabe der Energiesperre mit `powercfg /requests`.
- **PowerShell-Flag korrigiert** - Das Smoke-Test-Skript wandelt das hohe `ES_CONTINUOUS`-Bit explizit in `UInt32` um und läuft damit auch in Windows PowerShell 5.1.
- **Unplanmäßige Tray-Enden sichtbar** - Eine Sitzung ohne sauberen Abschlussmarker wird beim nächsten Start im Abschlussprotokoll der letzten 30 Ereignisse vermerkt. Erwartete Energieaktionen werden dabei nicht als Absturz fehlklassifiziert.

## [2026-08-12] - Live-Fenster zuverlässig öffnen

### 🪟 Bedienung

- **Position bleibt erhalten** - Die zuletzt gewählte Desktop-Position des Live-Fensters wird lokal gespeichert und beim nächsten Start wieder verwendet.
- **Keine Duplikate** - Ein erneutes Öffnen restauriert und fokussiert das vorhandene Fenster. Eine benannte Windows-Sperre verhindert parallele Live-Fenster auch bei schnellen Doppelstarts.

### 🔧 Diagnose

- **Fehler nicht mehr unsichtbar** - Der Tray wartet auf ein tatsächlich sichtbares Live-Fenster. Startfehler werden gemeldet und zusätzlich in `logs/ui-errors.log` festgehalten.

## [2026-08-11] - Wetter im Mond

### 🌡️ Live-Status

- **Temperatur in der Sichel** - Die aktuelle Temperatur des gewählten Wetterorts erscheint direkt in der freien Mondfläche.
- **Suchbarer Wetterort** - Ein kleines Hover-Symbol unten rechts öffnet ein Fenster im Stil des Abschlussprotokolls. Stadt oder Postleitzahl suchen, Treffer auswählen, fertig.
- **Unabhängig und fail-safe** - Wetter wird im Hintergrund aktualisiert und beeinflusst weder Herdr-Prüfung noch Ruhezeit oder Energieaktion.
- **Drittanbieter sauber dokumentiert** - README und `THIRD_PARTY_NOTICES.md` erklären Open-Meteo-Attribution, die freien Nichtkommerziell-Bedingungen und den notwendigen kommerziellen Plan.

## [2026-08-09] - Neustart setzt Nachtlauf zurück

### 🛡️ Sicherheit

- **Kein Nachtlauf über Neustarts hinweg** - Jeder Lauf speichert die WSL-Boot-ID. Nach einem Windows- oder WSL-Neustart werden ein liegen gebliebener Lauf und seine Warnungsdatei sicher zurückgesetzt.
- **Nachvollziehbarer Reset** - Der Reset landet im Diagnoseprotokoll und im begrenzten Abbruchverlauf mit dem Auslöser `system_restart`.
- **Gezielter Neustarttest** - Ein neuer Test deckt sowohl den Reset nach einer geänderten Boot-ID als auch den normalen Neustart des Wächterprozesses innerhalb derselben Sitzung ab.

## [2026-08-07] - Live-Fenster und öffentliche Oberfläche

### 🪟 Bedienung

- **Fenster beim Start öffnen** - Eine gespeicherte Tray-Option öffnet das Live-Fenster automatisch; standardmäßig bleibt die App weiterhin Tray-only.
- **Verschieben ohne Fummelei** - Zahlenkarten, Kopfzeile und Systemzeile lassen sich mit dem normalen Mauszeiger verschieben. Nur echte Bedienelemente bleiben klickbar.
- **Abschlussprotokoll im gleichen Stil** - Das Protokoll zeigt die letzten 30 Energieaktionen in einem frei verschiebbaren Fenster mit dauerhaft sichtbaren Minimieren- und Schließen-Schaltflächen.

### 🌍 Veröffentlichung

- **Bilinguale Oberfläche** - Das Abschlussprotokoll übersetzt jetzt auch Lauf-ID und Lesefehler zwischen Deutsch und Englisch.
- **Aktuelle Screenshots** - Die README verwendet die aktuelle deutsche und englische Live-Status-Ansicht.

## [2026-08-07] - Systemwerte im Live-Status

### 🎛️ Live-Überblick

- **Schmale Systemzeile** - CPU, GPU, VRAM, RAM und der verfügbare Grafikkarten-Wattwert erscheinen als kleine Anzeige unter dem Herdr-Dashboard.
- **Schnell lesbare Warnfarben** - Mittlere Auslastung wird pastellgelb, hohe Auslastung pastellrot. Das gilt auch für VRAM und GPU-Watt relativ zum Power-Limit.
- **Noch weniger Text** - Das VRAM-Symbol trägt seine GPU-Kennzeichnung selbst; die Zeile zeigt nur noch Größe und Auslastung.
- **Nur Anzeige** - Die Telemetrie läuft unabhängig vom Wächter und kann weder Ruhezeit, Warnfrist noch Energieaktion beeinflussen.
- **Fail-safe bei fehlender Hardware** - Nicht verfügbare Werte werden als `—` angezeigt; insbesondere wird kein Wattwert erfunden, wenn Windows keinen Sensor meldet.

---

## [2026-08-06] - Verdeckter Windows-Starter repariert

### 🔧 Zuverlässigkeit

- **Nachtlauf startet wieder wirklich** - Der Task Scheduler verwendet jetzt einen versteckten PowerShell-Starter statt einer fehleranfälligen VBS-Kette.
- **Keine falschen Erfolgsmeldungen** - Ein scharfgestellter Lauf wird erst dann überwacht, wenn der Hintergrundstarter WSL tatsächlich erreicht.
- **Regressionstest** - Der Test sichert, dass der Installationsskript weiterhin den PowerShell-Starter registriert.

---

## [2026-08-05] - Sicherheitsfixes aus Code-Review

### 🔧 Sicherheit

- **Beobachtung bleibt folgenlos** - Ein Beobachtungslauf kann auch über OK keine Windows-Energieaktion auslösen.
- **Abbruch gewinnt zuverlässig** - Abbruch, Bestätigung und der endgültige Abschluss sind gegenläufig synchronisiert.
- **Warnfenster fail-safe** - Nur ein explizites OK bestätigt. Schließen, Abbrechen oder ein Dialogfehler brechen sicher ab.
- **Task bleibt überwachbar** - Der versteckte VBS-Starter gibt Fehler von WSL an den Windows Task Scheduler weiter.
- **Tray sauber beenden** - Ein aktiver Nachtlauf wird beim vollständigen Beenden der Tray-App sicher abgebrochen.

---

## [2026-08-04] - Abschlussaktion wählbar

### 🎯 Highlights

- **Abschluss bewusst wählen** - Im Live-Fenster wählt der kleine Schalter direkt rechts neben „Herdr jetzt“: Stecker links für Energiesparmodus, Power-Symbol rechts für Herunterfahren.
- **Lauf bleibt eindeutig** - Die Auswahl wird beim Start des Nachtmodus gespeichert und kann währenddessen nicht versehentlich geändert werden.

### 🔧 Sicherheit

- **Gleiche Warnfrist, klare Aktion** - Beide Modi warten nach 5 Sekunden Ruhezeit 300 Sekunden. Energiesparen ruft dabei keinen Windows-Shutdown-Befehl auf.

### ✏️ Aktualisierungen

- **Countdown im Blick** - Die rote Warnphase zeigt ihre verbleibende Zeit direkt im Live-Fenster.
- **Warnfrist nach Wunsch** - Das Sekundenfeld neben dem Abschluss-Schalter setzt für den nächsten Nachtlauf eine Warnfrist zwischen 10 und 3.600 Sekunden.
- **Bewusst auch ohne laufende Arbeit** - Ein expliziter Start bei 0 arbeitenden Agenten beginnt nach der Ruhezeit den sicheren Countdown, statt still zu scheitern.
- **Offline mit Sicherheitsnetz** - Erst fünf Minuten ohne Internet starten dieselbe abbrechbare Warnfrist; kehrt die Verbindung zurück, wird sie abgebrochen.
- **Abschlussverlauf** - Der Installationsordner enthält unter `logs` einen auf 30 Ereignisse begrenzten CSV-Verlauf für angeforderte Schlaf- und Shutdown-Vorgänge.
- **Abbruch nicht mehr rätselhaft** - Ein zweiter, auf 30 Ereignisse begrenzter Verlauf nennt künftig die konkrete Quelle jedes manuellen Abbruchs.
- **Gespeicherte Warnfrist bleibt erhalten** - Start über Mond, Tray oder Standardskript überschreibt den im Sekundenfeld gewählten Wert nicht mehr mit 300 Sekunden.

---

## [2026-08-04] - Nachtlauf folgt aktueller Herdr-Arbeit

### 🎯 Highlights

- **Neue Arbeit zählt automatisch** - Der Nachtmodus prüft fortlaufend alle aktuell von Herdr gemeldeten Agenten statt einer alten Momentaufnahme.
- **Countdown bleibt sicher** - Startet während Ruhezeit oder Warnfrist neue Herdr-Arbeit, bricht der eigene Countdown ab und der Wächter beobachtet weiter.
- **Klarer Moon-Status** - Der große Mond rechts im Live-Fenster zeigt beim Darüberfahren verständlich, ob ein Nachtlauf aktiv ist.

### ✏️ Aktualisierungen

- **Sicherheitsvertrag** - Fehlende, blockierte oder unbekannte Herdr-Status verweigern weiterhin den Shutdown.
- **Dokumentation** - Systembild, Bedienung, Diagnose und Testmatrix auf die Live-Prüfung umgestellt.

### 🔧 Meta

- Modus: REFRESH
- Skill: documentation v6.0

---

## [2026-08-04] - Live-Kontrolle ergänzt

### 🎯 Highlights

- **Herdr-Live-Zahl im Blick** - Tooltip, Menü und ein optionales Statusfenster zeigen jetzt, wie viele Agenten Herdr erkannt hat und wie viele arbeiten.
- **Sicherer Schnell-Neustart** - Ein neuer Wächter wartet kurz auf die Sperre des Vorgängers, statt nach einem schnellen Stopp still zu scheitern.

### ✏️ Aktualisierungen

- **Bedienung und Zustände** - Live-Arbeit und die damalige eingefrorene Nachtlauf-Zielmenge wurden getrennt erklärt.
- **Betrieb und Fehlerdiagnose** - Sperren-Rennbedingung und ihr Diagnoseweg ergänzt.

### 🔧 Meta

- Modus: REFRESH
- Skill: documentation v6.0

---

## [2026-08-04] - Wartungsdokumentation erstellt

### 🎯 Highlights

- **Sicherer Wieder-Einstieg** - Architektur, Zustände, Betrieb und Auslieferung sind jetzt an einem Ort nachvollziehbar.

### 📄 Neue Dokumente

- **Systembild** - Erklärt die Trennung zwischen Tray-App, Windows-Task, WSL-Wächter und Herdr.
- **Bedienung und Zustände** - Ordnet jedem Menüpunkt, Symbolzustand und Popup eine eindeutige Wirkung zu.
- **Betrieb und Fehlerdiagnose** - Enthält sichere Status-, Stopp- und Logwege sowie die Erklärung gegen sichtbare Terminalfenster.
- **Entwicklung und Test** - Hält Sicherheitsinvarianten, Build-Orte und eine Shutdown-freie Testmatrix fest.

### 🔗 Querverweise

- Der maßgebliche Sicherheitscode ist aus der Dokumentation direkt mit dem WSL-Wächter verlinkt.
- Die Dokumentation verlinkt auf Herdr, Windows-Skripte und die Rust-Quellen, damit Ursache und Bedienung zusammen geprüft werden können.

### 🏗️ Struktur

- `INDEX.md` als Einstieg mit Vorher-Nachher, Entscheidungen und Lesepfaden angelegt.
- Einheitliche Navigation in allen Wartungsseiten ergänzt.

### 🔧 Meta

- Modus: CREATE
- Skill: documentation v6.0
