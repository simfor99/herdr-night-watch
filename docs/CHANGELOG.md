# Changelog - Herdr-Nachtwächter

> Alle wesentlichen Änderungen an dieser Dokumentation.
> Format: Game-Style mit Fokus auf Leser-Wirkung.

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
