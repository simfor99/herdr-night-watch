# Abschlussprüfung für übergeordnete Arbeit

Stand: 11. Oktober 2026. Quellen: `evaluate_work_guards()`, `evaluate()`,
`confirm_completion()` und `current_warning_id()` in
[`watcher/herdr-night-watch.py`](../watcher/herdr-night-watch.py),
[`src/notify.rs`](../src/notify.rs) und [`src/backend.rs`](../src/backend.rs).

## Wann ein Auftrag fertig ist

Herdr kann eine einzelne Claude-Antwort als beendet melden, während ein
übergeordneter Ablauf noch Aufgaben oder automatische Fortsetzungen erwartet.
Deshalb können zusätzlich unabhängige Abschlussprüfungen eingerichtet werden.
Nur wenn alle Prüfungen freigeben und Herdr keine aktive oder unklare Arbeit
meldet, beginnt die Ruhezeit. Die zusätzlichen Prüfungen gelten auch während
der Warnfrist und unmittelbar vor der Energieaktion. Sie blockieren auch den
Abschluss wegen eines Internetausfalls.

Ein Fehler, eine ungültige Antwort oder eine Zeitüberschreitung gibt niemals
eine Abschaltung frei. Ein ausdrücklich unbekannter Zustand beendet den
Nachtlauf ohne Energieaktion. Ein vorübergehender Abfragefehler setzt die
Ruhezeit zurück und wird später erneut geprüft.

## Allgemeiner Vertrag

`target_contract`, implementiert durch `load_work_guard_config()` und
`evaluate_work_guards()`: Die lokale Datei
`~/.local/state/herdr-night-watch/work-guards.json` enthält `schema_version`
mit Wert `1` und die Liste `guards`. Jede Prüfung hat eine eindeutige `id`
und `command` als Liste einzelner Programmargumente. Es wird keine Shell
aufgerufen. Die Konfiguration wird beim Start in `active-run.json` eingefroren;
Änderungen oder das Entfernen der Konfigurationsdatei verändern einen bereits
gestarteten Lauf nicht.

Jeder Aufruf muss innerhalb von fünf Sekunden erfolgreich JSON liefern:
`schema_version` ist `1`, `status` ist `ready`, `active` oder `unknown`.
`detail` darf eine kurze Diagnose enthalten. `ready` gibt nur diese einzelne
Prüfung frei. Andere Prüfungen und Herdr müssen ebenfalls zustimmen. Die
Ausgabe wird bei jeder Entscheidung frisch abgerufen und nicht zwischengespeichert.

## Firstmate-Anbindung

[`watcher/firstmate_work_guard.py`](../watcher/firstmate_work_guard.py) bekommt
das überwachte Firstmate-Verzeichnis über `--home`. Es liest dessen Zustand
ohne Änderungen oder Nachrichten an andere Sessions.

Die verwendeten Firstmate-Quellen sind `bin/fm-supervision-lib.sh`, Funktion
`fm_supervision_status()`, und `docs/turnend-guard.md`, Abschnitt
„Supervision need“: Vorhandene `state/*.meta` zählen als noch registrierte
Aufgaben; eine nicht leere `state/.wake-queue` bedeutet eine ausstehende
Fortsetzung. Beides verhindert die Abschaltung, auch wenn Herdr sämtliche
Agenten vorübergehend als `done` meldet.

Ein laufender Beobachter, registrierte Beobachtungsquellen, die Existenz eines
Plans oder eine reine Lebenszeichen-Datei gelten allein nicht als ausstehende
Arbeit. Abgeschlossene Aufgaben muss Firstmate selbst aus seinem aktiven
Bestand entfernen; der Nachtwächter interpretiert keine Terminaltexte und räumt
keine Firstmate-Dateien auf. Ein fehlender oder nicht lesbarer Firstmate-Zustand
wird als unbekannt gemeldet. Für andere Firstmate-Verzeichnisse sind eigene
Prüfungen einzurichten; die Anbindung sucht nicht selbst nach weiteren Projekten.

## Warnfenster und Bestätigung

Jede Abschaltwarnung hat eine eigene `warning_id`. Das Warnfenster fragt deren
Gültigkeit ungefähr einmal pro Sekunde über `--warning-status` ab. Wird die
Warnung zurückgenommen, ersetzt oder beendet, schließt ausschließlich das
zugehörige eigene Fenster. Das automatische Schließen bricht keinen neuen
Nachtlauf ab. Bei einem Lesefehler bleibt das Fenster zunächst bestehen.

„OK“ und „Abbrechen“ übermitteln die Kennung der angezeigten Warnung. Ein altes
Fenster kann dadurch keine neue Warnung bestätigen oder abbrechen. Bei „OK“
prüft der Wächter unter seiner Abschlusssperre erneut die aktuellen
Arbeitszustände und die verbleibende Frist. Aktive oder unklare Arbeit nimmt
die Warnung zurück; es wird keine Energieaktion angefordert.

## Sichere Tests

`watcher/test_work_guards.py` prüft Antwortpausen, ausstehende Firstmate-Aufgaben,
Fortsetzungen, Freigaben, Abfragefehler, eingefrorene Konfiguration und alte
Dialogkennungen. Sämtliche Windows-Energieaktionen sind dabei ersetzt.
Die Rust-Tests in `src/notify.rs` prüfen zusätzlich an unsichtbaren eigenen
Testfenstern, dass ausschließlich der passende Dialog ein Schließsignal erhält.

Eine echte Abschaltung ist kein automatischer Test.
