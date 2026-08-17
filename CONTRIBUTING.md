# Mitmachen

Danke für Verbesserungen am Herdr-Nachtwächter.

Bitte beschreibe in einem Issue oder Pull Request:

- welches Verhalten sich ändert,
- wie du es unter Windows und WSL geprüft hast,
- ob der Shutdown-, Energiespar- oder Abbruchpfad betroffen ist.

Vor einem Pull Request:

```bash
cargo fmt --check
cargo test --target x86_64-pc-windows-gnu
cargo clippy --target x86_64-pc-windows-gnu -- -D warnings
python3 -m unittest -v watcher/test_herdr_night_watch.py
```

Bitte niemals Laufzeitdateien, Logs, persönliche Pfade, Tokens oder Zugangsdaten committen.

## README-Bilder und Release-Assets

README-Screenshots werden als öffentliche Release-Assets eingebettet. Relative
`docs/images/...`-Pfade und GitHub-`/raw/...`-Pfade dürfen nicht verwendet werden:
GitHub kann diese Pfade in der gerenderten Repository-Ansicht als `404` oder
Broken Image ausliefern. Die PNG-Dateien müssen im Repository normale Dateien
mit Modus `644` sein, und das Release-Asset muss vor dem README-Push existieren.

Vor dem Push prüfen:

```bash
python3 tools/check_readme_images.py
```

Der Check prüft zuerst, dass die kanonischen Quelldateien existieren, von Git
erfasst sind und den normalen Dateimodus `644` haben. Danach lädt er jede im
README eingebettete Release-URL, folgt dem Redirect und prüft, dass tatsächlich
ein PNG zurückkommt. Anschließend die gerenderte Repository-Seite einmal in
Chrome mit `Strg + F5` öffnen.
