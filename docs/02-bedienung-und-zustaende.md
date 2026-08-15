> [Zurück zum Index](./INDEX.md) | [Systembild](01-systembild.md) | **Bedienung und Zustände** | [Betrieb und Fehlerdiagnose](03-betrieb-und-fehlerdiagnose.md) | [Entwicklung und Test](04-entwicklung-und-test.md)

# Bedienung und Zustände

> **Zweck:** Die sichtbare Bedienung soll immer dieselbe Bedeutung wie der Sicherheitszustand haben.
> **Quellstand:** 2026-08-04

---

## Was ein Klick tatsächlich bedeutet

Die Tray-App ist für eine klare Abendroutine gebaut: Nachtmodus wählen, Zustand am Mondsymbol erkennen und bei Bedarf abbrechen. Sie entscheidet aber nie allein, ob der Rechner herunterfahren darf. Jeder Start schaltet im WSL-Wächter die Prüfung aller aktuell von Herdr gemeldeten Agenten ein.

Das Menü sperrt Start, Beobachtung, Demo und Stopp passend zum aktiven Zustand. So kann nicht versehentlich ein zweiter Lauf über den ersten gelegt werden. Der aktuelle Zustand steht als erste, nicht anklickbare Zeile im Menü und außerdem im Tooltip des Tray-Symbols.

## Menü und Wirkung

| Menüeintrag | Wirkung | Risiko- und Sicherheitsgrenze |
|---|---|---|
| `Nachtmodus starten` | Schaltet die Prüfung aller aktuell von Herdr gemeldeten Agenten ein und erlaubt späteren echten Abschluss. | Ein bewusster Start ist auch ohne gerade arbeitenden Agenten erlaubt; dann beginnt die Ruhezeit unmittelbar. |
| `Nur beobachten` | Gleiche Erfassung, aber mit `dry_run=true`. | Es wird nie eine Windows-Energieaktion ausgeführt, auch nicht nach einem Klick auf OK. |
| `Stopp und Shutdown abbrechen` | Löscht aktiven Lauf, stoppt den Task und versucht `shutdown.exe /a`. | Es betrifft nur eine noch gültige, vom Wächter erstellte Warnung. |
| `Demo: Abschluss simulieren` | Simuliert Ruhezeit und Warnung in wenigen Sekunden. | `demo=true` und `dry_run=true`; kein Herdr-Check und kein echter Shutdown. |
| `Live-Status öffnen` | Öffnet ein kleines bewegliches Statusfenster mit einem klickbaren Mond, Abschluss-Schalter, Sekundenfeld und Temperaturanzeige. | Linksklick auf das Tray-Symbol öffnet dasselbe Fenster, Rechtsklick zeigt dieses Menü. Klick auf den grauen Mond startet, Klick auf einen farbigen Mond stoppt über denselben sicheren Pfad wie das Tray-Menü. Der Schalter wählt links mit Stecker Energiesparmodus und rechts mit Power-Symbol Herunterfahren. Das Feld legt die Warnfrist von 10 bis 3.600 Sekunden für den nächsten Nachtlauf fest. Nach einem Klick erscheint drei Sekunden ein Toast; das X schließt nur das Fenster. |
| `Live-Fenster beim Start öffnen` | Speichert, ob die Tray-App beim Start zusätzlich das Live-Fenster öffnen soll. | Der Standard ist Tray-only. Die Einstellung startet keinen Nachtlauf und verändert keine Wächterentscheidung. |
| `Protokoll öffnen` | Öffnet `watch.log` im Editor. | Dient nur der Diagnose. Das obere Protokoll-Symbol im Live-Fenster öffnet stattdessen das gestaltete Abschlussprotokoll mit den letzten 30 Energieaktionen. |
| `Wetterort ändern` | Wird über das kleine Wettersymbol unten rechts im Live-Fenster geöffnet. Sucht Städte oder Postleitzahlen und speichert den ausgewählten Treffer lokal. | Die Temperatur ist rein informativ. Netzwerkfehler führen zu `—` oder zum letzten Wert und können den Nachtwächter nie beeinflussen. |
| `Mit Windows starten` | Registriert die Tray-App für den Benutzer-Login. | Scharfstellt niemals automatisch einen Nachtlauf. |
| `Tray-App beenden` | Beendet die Bedienoberfläche. | Ein aktiver Nachtlauf wird vorher sicher abgebrochen, damit keine sichtbare Warnmöglichkeit verloren geht. |

## Zustandsautomat

Der Zustand ist nicht nur eine Farbe, sondern eine Aussage über den nächsten erlaubten Schritt. `backend::status()` übersetzt die JSON-Datei des Wächters in `WatchStatus`; [`src/tray.rs`](../src/tray.rs) baut daraus Menü, Tooltip und Symbol.

```text
Aus
  | Nachtmodus / Nur beobachten / Demo
  v
Aktiv: Herdr meldet arbeitende Agenten
  | Herdr meldet keine arbeitenden Agenten
  v
Ruhezeit läuft
  | ein Herdr-Agent arbeitet wieder -> zurück zu Aktiv
  | Ruhezeit erfüllt, direkte zweite Prüfung erfolgreich
  v
Warnfrist
  | Abbrechen / Agent wird wieder aktiv -> Aus bzw. letzter Lauf
  | Frist abgelaufen -> shutdown_scheduled
  v
Letzter Lauf
```

| Symbolfarbe | Zustand | Bedeutung |
|---|---|---|
| Grau | Aus oder letzter Lauf beendet | Kein aktiver Nachtlauf |
| Grün | Nachtmodus, Arbeit läuft | Mindestens ein aktuell gemeldeter Herdr-Agent ist aktiv |
| Blau | Nur beobachten | Gleicher Prüfablauf, aber ohne Shutdown |
| Gelb | Ruhezeit | Herdr meldet aktuell keine arbeitenden Agenten, Zeit wird bestätigt |
| Rot | Warnfrist | Shutdown ist vorbereitet und noch abbrechbar |

## Warnfenster und Abbrechen

Im echten Nachtmodus beginnt nach fünf Sekunden erfolgreicher Ruhezeit die im Sekundenfeld gewählte Warnfrist. Gleichzeitig schreibt der Wächter `shutdown-warning.json` mit der exakten Ablaufzeit und der beim Start festgelegten Abschlussaktion. Im Modus Herunterfahren plant er sofort `shutdown.exe /s /t <Warnfrist>`. Im Energiesparmodus wartet der Wächter dieselbe Warnfrist und fordert Windows anschließend zum Schlafen auf. Sobald die Tray-App diese Phase erkennt, zeigt [`notify::completion_notice()`](../src/notify.rs) den passenden Windows-Dialog aktiv im Vordergrund und über anderen Fenstern an. Der Mond im Live-Fenster wird währenddessen rot.

Während der gesamten Warnfrist prüft der Wächter Herdr weiter. Meldet ein Agent wieder `working`, bricht er die eigene Warnung ab, setzt die Ruhezeit auf null und beginnt nach bestätigter neuer Ruhezeit eine vollständig neue Warnfrist. Direkt vor der Energieaktion erfolgt zusätzlich eine letzte Herdr-Prüfung. Dadurch wird auch die kleine Grenzsekunde am Ende des Countdowns fail-safe behandelt.

Die Schaltfläche **OK** ist eine bewusste Sofortbestätigung: Sie führt die beim Start gewählte Aktion direkt aus. Bei Energiesparmodus fordert der Wächter Windows unmittelbar zum Schlafen auf; beim Herunterfahren wird der eigene geplante Shutdown durch einen sofortigen ersetzt. Die Schaltfläche **Abbrechen** ruft `backend::stop()` auf, der wiederum `Stop-HerdrNightWatch.ps1` ausführt. Dieser Pfad löscht den aktiven Lauf und verwendet `shutdown.exe /a` nur dann, wenn der Wächter selbst einen Shutdown angesetzt hatte. Eine Energiespar-Warnung wird ohne diesen Windows-Befehl abgebrochen. Ohne Klick läuft die Warnfrist weiter und Windows führt die beim Start des Nachtlaufs gewählte Aktion aus.

Der normale OK-Klick dient nur zum Schließen der Information. Er bestätigt keinen zusätzlichen Shutdown, denn dieser wurde bereits durch den Wächter angesetzt. Deshalb darf die Dialoglogik nicht zu einem zweiten `shutdown.exe`-Aufruf erweitert werden.

## Demo ist eine Sicherheitsprobe, keine verkürzte Nacht

Die Demo startet direkt einen künstlichen Lauf mit `targets=[]`, `demo=true`, `dry_run=true`, acht Sekunden Ruhezeit und 15 Sekunden Warnfrist. `evaluate()` meldet für diesen Sonderfall sofort „alle simulierten Agenten fertig“. Der Wächter schreibt zwar die Warnphase und die Tray-App zeigt den Dialog, `schedule_shutdown()` protokolliert bei `dry_run` aber lediglich den hypothetischen Shutdown.

Damit lässt sich das sichtbare Ende des Ablaufs testen, ohne Herdr-Arbeit zu verändern und ohne Windows zu gefährden. Eine Änderung an der Demo muss diese drei Schranken erhalten: `demo=true`, `dry_run=true` und keine Ausführung von `shutdown.exe /s`.

## Bekannte Bedienungsgrenzen

Der Nachtlauf beobachtet fortlaufend den aktuellen Herdr-Status. Beginnt nach dem Scharfstellen neue Arbeit in einem anderen Pane, gehört sie automatisch dazu und setzt eine laufende Ruhezeit zurück. Beginnt sie während der Warnfrist, bricht der Wächter seinen eigenen Windows-Countdown ab und beobachtet weiter. Nicht von Herdr verwaltete Shell-Prozesse sind absichtlich außerhalb der Entscheidung.

Fällt die Internetverbindung aus, zählt das nicht sofort als „fertig“. Erst nach fünf Minuten ohne erfolgreiche Antwort von zwei unabhängigen Verbindungschecks beginnt die normale Warnfrist. Das Warnfenster erklärt den Grund. Kommt Internet während des Countdowns zurück, wird der Countdown abgebrochen. Im Installationsordner unter `logs\completion-history.csv` stehen die letzten 30 tatsächlich angeforderten Energiespar- oder Shutdown-Vorgänge sowie erkannte unplanmäßige Tray-Enden mit Datum, Uhrzeit und Auslöser.

Wenn das Tray-Symbol nicht direkt sichtbar ist, liegt es möglicherweise im ausgeklappten Bereich der Windows-Taskleiste hinter dem Pfeil `^`. Das ist ein Windows-Verhalten und kein Zeichen, dass der Wächter gestoppt wurde.

Ein Windows- oder WSL-Neustart beendet einen aktiven Nachtlauf sicher. Der Wächter vergleicht beim nächsten Statusaufruf die gespeicherte Boot-ID mit der aktuellen. Bei einer Änderung löscht er eine eventuell alte Warnung, protokolliert den Reset als `system_restart` und meldet wieder „kein Nachtlauf aktiv“. Ein neuer Nachtlauf muss bewusst gestartet werden.

Während `Aktiv`, `Ruhezeit` oder `Warnfrist` setzt die Tray-App außerdem eine Windows-Ausführungsanforderung gegen automatischen Leerlauf-Energiesparmodus. Das schützt vor einem von Windows gestarteten Schlaf, solange Herdr noch arbeitet. Diese Anforderung wird beim Stoppen und beim Beenden der Tray-App aufgehoben; die vom Nachtwächter selbst bestätigte Energieaktion wird nicht blockiert.

## Live-Zahl und Nachtlauf-Zahl

Der kompakte Tooltip lautet zum Beispiel `Aus · Herdr 4/10 aktiv`. Das ist die aktuelle Zahl aller von Herdr gemeldeten Agenten und beantwortet die Kontrollfrage „arbeitet Herdr gerade wirklich parallel?“, ohne an der Windows-Zeichengrenze abgeschnitten zu werden. Im laufenden Nachtmodus ist genau diese aktuelle Herdr-Sicht auch die Grundlage der Überwachung.

Das Fenster ist ein normales Windows-Fenster: frei verschiebbar, minimierbar und über das X schließbar. Es ist absichtlich kompakt, aber mit einem ruhigen Außenabstand von 20 Pixeln: Live-Zahlen und großer, klickbarer Mond stehen gemeinsam auf einer Zeile und haben dieselbe optische Höhe. Rechts neben „Herdr jetzt“ liegen der kleine Schalter für die Abschlussaktion und das Sekundenfeld für die Warnfrist. Links Stecker bedeutet Energiesparmodus, rechts Power-Symbol Herunterfahren. Das Sekundenfeld akzeptiert 10 bis 3.600 Sekunden und speichert beim Verlassen des Feldes. Während ein Nachtmodus läuft, sind beide bewusst gesperrt, weil ihre Werte dafür schon sicher gespeichert wurden. Rot zeigt die Warnfrist; direkt unter dem Mond läuft dann ein sichtbarer Minuten-und-Sekunden-Countdown. Grau bedeutet kein Nachtlauf und ein Klick startet den Nachtmodus. Grün bedeutet aktiver Nachtmodus und ein Klick stoppt ihn. Blau und gelb kennzeichnen Beobachtung und Ruhezeit und lassen sich ebenfalls sicher stoppen. Nach einem Klick wechselt der Mond sofort in den erwarteten Zustand, während der sichere Hintergrundschritt noch bestätigt wird. Nach erfolgreicher Bestätigung erscheint unten für drei Sekunden ein Toast mit „Nachtmodus aktiv“ oder „Nachtmodus deaktiviert“. Beim Darüberfahren zeigt der Mond den aktuellen Zustand und darunter die Wirkung des nächsten Klicks. Das Fenster kann manuell über Tray oder Linksklick geöffnet werden und lässt sich zusätzlich über die Startoption automatisch mit der Tray-App öffnen; beim Schließen bleiben Tray-App und Wächter unverändert aktiv.

Der gespeicherte Sekundenwert ist die maßgebliche Einstellung: Start über Mond oder Tray übernimmt ihn unverändert. Das Windows-Startskript übergibt nur dann eine Warnfrist, wenn es ausdrücklich mit einem eigenen Sekundenwert aufgerufen wird.

Die Temperatur im Mond kommt vom ausgewählten Wetterort. Ein kleiner Wetterknopf am unteren rechten Rand ist erst beim Darüberfahren sichtbar. Ein Klick öffnet das Suchfenster im gleichen ruhigen Glasstil wie das Abschlussprotokoll. Die Abfrage läuft alle zehn Minuten im Hintergrund; der Nachtwächter bleibt auch ohne Internet vollständig funktionsfähig.
