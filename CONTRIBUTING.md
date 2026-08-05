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
