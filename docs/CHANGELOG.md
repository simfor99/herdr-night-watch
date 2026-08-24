# Changelog - Herdr-Nachtwächter

> Alle wesentlichen Änderungen an dieser Dokumentation.
> Format: Game-Style mit Fokus auf Leser-Wirkung.

## [2026-08-24] - Analoge Uhr im Mond

### 🕰️ Live-Status

- **Zeit direkt im Mond** - Stunden- und Minutenzeiger leuchten sanft grün. Vom
  schmalen Ansatz werden sie zunächst etwas breiter und laufen erst im letzten
  Stück nadelförmig zusammen. Eine weiße Innenkontur und eine feine schwarze
  Außenkontur halten beide Zeiger vor jeder Mondphase lesbar. Der Minutenzeiger
  reicht nahe an den äußersten Mondrand; auch der Stundenzeiger ragt sicher über
  die Temperaturanzeige hinaus. Die Temperatur bleibt als oberste Ebene
  vollständig sichtbar.
- **Feine Stundenorientierung** - Die blassen Ziffern `12`, `3`, `6` und `9`
  ersetzen die vier breiten Viertelstunden-Striche. Acht kurze, zwei Pixel
  breite Zwischenstriche bleiben erhalten. Russo One aus Google Fonts liefert
  die nun 50 Prozent größeren, kräftigen Ziffern mit dunkler Kontur. Sie
  skalieren ohne Qualitätsverlust und schweben außerhalb der Mondscheibe im
  erweiterten Hof; der Mond sitzt zum Ausgleich 13,5 Pixel weiter rechts.
- **Mudmaster-artiger Sekundenzeiger** - Ein sehr schmaler, hellerer und blasser
  orangefarbener Sekundenzeiger reicht als lange Nadel bis knapp über den
  Mondrand und ist standardmäßig eingeschaltet. Per
  Rechtsklick auf die Uhr lässt er sich unmittelbar ein- oder ausblenden; die
  Auswahl bleibt für spätere Starts gespeichert.

## [2026-08-24] - Tray hält Windows durchgehend wach

### 🛡️ Energieschutz

- **Kein Leerlauf-Schlaf bei laufendem Tray** - Die Tray-App hält Windows jetzt
  während ihrer gesamten Laufzeit wach, auch wenn kein Nachtmodus aktiv ist.
  Der Bildschirm darf weiterhin ausgehen und bewusste Energieaktionen bleiben
  möglich.
- **Saubere Freigabe beim Beenden** - Die Windows-Anforderung ist direkt an die
  Lebensdauer der Tray-App gebunden und wird beim Verlassen automatisch
  freigegeben.
- **Fehler bleibt fail-closed** - Lehnt Windows die Energiesperre beim
  Tray-Start ab, läuft die primäre Tray-App nicht weiter. Ein bereits aktiver
  oder nicht sicher abfragbarer Nachtlauf wird vorher über den normalen
  Stopp-Pfad beendet.

## [2026-08-22] - Live-Fenster übersteht den Morgen nach dem Shutdown v0.1.24

### 🪟 Live-Status

- **Kein weißes Blitzen nach dem Boot** - Öffnet der Nachtwächter das
  Live-Fenster direkt bei der Windows-Anmeldung, wartet er jetzt kurz, bis
  Grafiktreiber und Bildschirme wach sind. Das beendet die kurzen weißen
  Fenster, die nach einem Nachtwächter-Shutdown sofort wieder verschwanden.
- **Verklemmter Erstversuch wird beseitigt** - Blieb der erste Fensterprozess
  nach dem Boot ohne sichtbares Fenster hängen, blockierte er dauerhaft jede
  weitere Öffnung. Der Tray erkennt so einen Altprozess jetzt, räumt ihn weg
  und startet einen frischen Versuch.
- **Fenster bleibt im Bild** - Die gespeicherte Fensterposition wird beim
  Öffnen geprüft. Fehlt der Monitor, auf dem das Fenster zuletzt lag, wandert
  es auf den nächsten vorhandenen Bildschirm und merkt sich den neuen Platz.
- **Kein Waisenfenster beim Beenden** - Beendest du die Tray-App, nimmt sie das
  Live-Fenster jetzt auch dann mit, wenn das Öffnen längst abgeschlossen war.

## [2026-08-21] - Medienanzeige ohne Dauerlast v0.1.23

### 🎵 Live-Status

- **Spotify bleibt leichtfüßig** - Der Nachtwächter reagiert auf Änderungen der
  Windows-Mediensitzung, statt den Medienmanager im Dreiviertelsekundentakt
  neu anzufragen. Das entlastet insbesondere Spotify und den Windows-
  Sitzungs-Manager bei offenem Live-Fenster.
- **Timeline bleibt flüssig und klickbar** - Die Anzeige rechnet den
  sichtbaren Fortschritt lokal fort und holt Windows-Informationen nur bei
  echten Änderungen sowie in einem sparsamen Sicherheitsintervall nach.
- **Verbindung bleibt robust** - Temporäre Windows-Fehler halten die zuletzt
  bekannte Medienanzeige fest und werden zeitnah erneut versucht. Beim
  Schließen des Live-Fensters werden die Medien-Ereignisse sauber abgemeldet.

## [2026-08-20] - Neustart setzt den Nachtlauf wirklich zurück

### 🛡️ Sicherheit

- **Kein roter Mond nach dem Boot** - Ein abgeschlossener Lauf wird nach einer
  neuen WSL-/Windows-Bootmarke aus dem aktuellen Zustand entfernt. Die
  Abschlusschronik bleibt erhalten, aber der nächste Start beginnt wieder im
  normalen Zustand.

### 🪟 Bedienung

- **Live-Fenster bleibt offen** - Wenn eine zweite EXE-Instanz den Status öffnet,
  gehört das Fenster jetzt dem bereits laufenden Tray-Prozess. Die kurzlebige
  Zweitinstanz kann daher nicht mehr das sichtbare Fenster nach wenigen
  Millisekunden mit sich schließen.
- **Autostart repariert Altinstallationen** - Der Tray ergänzt beim Start den
  `--autostart`-Marker in einem älteren Windows-Run-Eintrag, damit die
  Einstellung für „Live-Fenster beim Start öffnen“ zuverlässig greift.

## [2026-08-19] - Abschlussprotokoll zeigt den echten Shutdown

### 🪟 Bedienung

- **Gestern erscheint wieder** - Der Wächter schreibt Energieaktionen in denselben Ordner, den das Abschlussprotokoll liest. Ein Herunterfahren landet nicht mehr unsichtbar unter Public.

## [2026-08-19] - Fertig-Halo bleibt knackig

### 🪟 Bedienung

- **Alter Halo, dünner Saum** - Fertige Agenten behalten den warmen gelben Schein um die Zahl. Der helle Rand um die Ziffer bleibt auch im Zoom nur ein Bildschirmpixel dick.

## [2026-08-19] - Temperatur bleibt auf dem Halbmond lesbar

### 🪟 Bedienung

- **Zahl mit Rand** - Die Temperatur sitzt mittig im Mond. Nur auf der hellen Sichel liegt ein ein Pixel breiter Rand in der Hintergrundfarbe, kein gestapelter Schatten mehr.

## [2026-08-19] - Live-Fenster beim EXE-Start v0.1.21

### 🪟 Bedienung

- **Doppelklick öffnet den Monitor** - Startet man die EXE und der Tray läuft noch nicht, geht das Live-Fenster direkt mit auf. Läuft der Tray schon, holt ein weiterer Doppelklick nur das Fenster nach vorn.
- **Klick trifft das echte Fenster** - Ein Einfach- oder Doppelklick auf den Mond startet oder holt das Live-Fenster. Das unsichtbare Tray-Hilfsfenster wird nicht mehr mit dem Live-Fenster verwechselt.
- **Kein Fenster ohne Mond** - Beendet man die Tray-App, schließt sich das Live-Fenster mit.

## [2026-08-19] - Live-Fenster geht mit dem Tray

### 🪟 Bedienung

- **Kein Fenster ohne Mond** - Beendet man die Tray-App, schließt sich das Live-Fenster mit. Es bleibt nicht mehr allein auf dem Schirm stehen.

## [2026-08-19] - Mondklick öffnet das Live-Fenster

### 🪟 Bedienung

- **Klick trifft das echte Fenster** - Ein Einfach- oder Doppelklick auf den Mond startet oder holt das Live-Fenster. Das unsichtbare Tray-Hilfsfenster wird nicht mehr mit dem Live-Fenster verwechselt.

## [2026-08-19] - EXE trägt den Mond

### 🪟 Bedienung

- **Eigenes Dateisymbol** - Die Windows-EXE zeigt denselben Material-Mond wie das Tray, nicht mehr das Standard-Rust-Icon.

## [2026-08-19] - Verstecktes Live-Fenster nach Neustart

### 🪟 Bedienung

- **Fenster kommt wieder** - Nach einem Windows-Neustart wird ein schon großes, aber verstecktes Live-Fenster eingeblendet. Das 4-mal-4-Pixel-Dummy von eframe zählt nicht mehr als Live-Fenster.

## [2026-08-18] - Skalierungsvorschau bleibt lesbar

### 🪟 Bedienung

- **Zahlen bleiben sichtbar** - Beim Verkleinern über den Anfasser bleibt das aktuelle Fenster stehen. Die kleinere Zielfläche liegt darin, Prozentwert und Pixelmaß werden nicht mehr vom schrumpfenden Fenster überdeckt.

## [2026-08-18] - Live-Fenster bleibt gezeichnet

### 🪟 Bedienung

- **Kein weißes Aufblitzen mehr** - Das Tray holt das Live-Fenster erst nach dem ersten echten Zeichenframe nach vorn. Ein noch verstecktes Vorbereitungsfenster wird nicht mehr vorzeitig eingeblendet.
- **Volle Deckkraft bleibt undurchsichtig** - Bei 100 Prozent Transparenz wird der Windows-Layered-Stil nicht mehr gesetzt. Glow kann das Fenster dadurch dauerhaft zeichnen statt nach einem kurzen Flash weiß zu bleiben.

## [2026-08-17] - Medienzeile und Live-Layout v0.1.19

### 📋 Abschlussprotokoll

- **Energie und Diagnose getrennt** - Das Abschlussprotokoll trennt Energieaktionen von Tray- und Diagnoseereignissen. Beide Bereiche zeigen jeweils bis zu 30 Einträge und übersetzen Beschriftungen sowie Meldungen vollständig ins Englische.

### 📊 Live-Dashboard

- **Fertige Agenten sichtbar hervorgehoben** - Sobald mindestens ein Agent fertig gemeldet ist, werden Zahl und Beschriftung grün dargestellt. Die Zahl erhält zusätzlich eine feine helle Kontur und einen warmen gelben Außen-Glow, die beide mit der Fenstergröße mitskalieren. Bei null kehren beide zu Grau zurück.

### 🎵 Live-Status

- **Kompakte Medienanzeige** - Aktueller Interpret und Songtitel erscheinen im Footer, wenn Windows eine Mediensession meldet.
- **Dotted Timeline** - Die Wiedergabe wird als platzsparende Punktzeile dargestellt. Hover zeigt die Sprungvorschau, ein Klick springt an diese Stelle, sofern die Session dies unterstützt.
- **Unabhängig vom Wächter** - Medienmetadaten und Wiedergabeposition beeinflussen weder Herdr-Überwachung noch Ruhezeit oder Energieaktion.

### 🎛️ Layout

- **Feinabgleich abgeschlossen** - Mondhalo, KPI-Zeile, Wattwert und Timeline sind auf gemeinsame rechte Abschlusslinien ausgerichtet.

### 🌙 Mondphase

- **Mondphase aus dem Wetterort** - Die Open-Meteo-Abfrage liefert zusätzlich die aktuelle tägliche Mondphase. Das Mondsymbol zeigt jetzt je nach Phase Sichel, Viertel, zunehmenden oder abnehmenden Mond, Vollmond oder Neumond.

### 🖼️ Live-Fenster

- **Proportionale freie Skalierung** - Das Hauptfenster kann über einen dezenten Griff unten rechts frei zwischen 75 und 1.000 Prozent vergrößert oder verkleinert werden. Während des Ziehens bleibt das Layout stabil und zeigt nur eine Vorschau; beim Loslassen wird die globale egui-Skalierung einmalig angewendet. Mond, KPI-Karte, Systemwerte, Medienzeile und Timeline bleiben zusammen. Die Einstellung bleibt gespeichert und lässt sich per Rechtsklick auf 100 Prozent zurücksetzen.
- **Temperatur bleibt lesbar** - Die Temperatur wandert in den dunklen Bereich der Mondgrafik und bleibt bei Vollmond kontrastreich zentriert.
- **Fail-safe** - Wenn die Phase noch nicht vom Wetterdienst vorliegt, verwendet die Anzeige eine lokale astronomische Näherung. Nachtwächter, Countdown und Energieaktion bleiben vollständig unabhängig.

## [2026-08-16] - Neustart- und Autostartstabilität v0.1.18

### 🛡️ Sicherheit

- **Kein alter Nachtlauf nach einem Boot-Rennen** - Der Wächter wartet beim Start auf vollständige WSL- und Windows-Boot-Marken und verwendet dieselbe bestätigte Marke für Reset und Überwachung.
- **Sicheres Abbrechen bei fehlender Boot-Marke** - Sind die Marken nicht verfügbar, startet keine Überwachung auf Basis eines möglicherweise alten Zustands.

### 🪟 Bedienung

- **Live-Fenster beim Windows-Start robuster** - Der automatische Start wartet kurz auf die Desktop-Umgebung und wiederholt einen frühen, unsichtbaren Fensterstart bis zu drei Mal.
- **Bessere Diagnose** - Auch Fehler beim verzögerten Autostart und die eigentliche Ursache eines Live-Status-Prozessfehlers landen in `logs/ui-errors.log`.

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
