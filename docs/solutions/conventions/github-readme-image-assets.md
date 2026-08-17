---
title: GitHub-README-Bilder über Release-Assets absichern
date: 2026-08-17
category: docs/solutions/conventions
module: github-readme-assets
problem_type: convention
component: documentation
severity: medium
applies_when:
  - "README-Bilder oder Screenshots werden in ein öffentliches GitHub-Repository aufgenommen"
  - "Ein README-Bild wurde lokal geprüft, ist aber in der gerenderten GitHub-Seite defekt"
tags:
  - github
  - readme
  - screenshots
  - release-assets
  - image-validation
---

# GitHub-README-Bilder über Release-Assets absichern

## Kontext

Ein lokaler relativer Bildpfad kann in einer GitHub-README syntaktisch korrekt
aussehen und trotzdem in Chrome als Broken Image enden. In diesem Projekt
lieferte GitHubs gerenderte `/raw/main/...`-Pfad für die PNGs `404`, obwohl die
Dateien über die GitHub-API im Repository vorhanden waren. Direkte Raw-URLs
waren außerdem von der Raw-Auslieferung und deren Rate Limits abhängig.

## Leitlinie

Öffentliche README-Screenshots werden als Release-Assets hochgeladen. Die
README verweist anschließend auf die stabile Release-URL:

```markdown
![Deutscher Live-Status](https://github.com/OWNER/REPO/releases/download/TAG/live-status-de.png)
```

Die Bilddateien bleiben zusätzlich im Repository als Quelle und müssen als
normale Dateien (`100644`) gespeichert und von Git erfasst sein. Der Check
`tools/check_readme_images.py` prüft beides: Er kontrolliert die lokalen
Quelldateien und lädt jede README-Bild-URL mit einem echten HTTP-GET, folgt dem
Release-Redirect und validiert die PNG-Signatur.

## Warum das wichtig ist

Damit wird zwischen drei Dingen unterschieden: Die Datei existiert im Git,
GitHub rendert den Markdown-Tag als `<img>`, und der Browser kann die
Bildquelle tatsächlich laden. Nur die Kombination aus allen drei Belegen ist
für eine öffentliche README ausreichend.

## Wann das gilt

- nach jedem Austausch eines README-Screenshots;
- vor jedem Push, der README-Bilder oder ein Release verändert;
- wenn GitHub einen Bildlink statt eines sichtbaren Bildes zeigt.

## Beispiele

Der verbindliche Ablauf ist:

1. PNG im Repository aktualisieren und Dateimodus `644` prüfen;
2. Release-Asset hochladen;
3. README auf die Release-Asset-URL setzen;
4. `python3 tools/check_readme_images.py` ausführen. Der Check schlägt auch
   fehl, wenn versehentlich wieder ein relativer oder nicht aus einem Release
   stammender Bildpfad im README landet;
5. GitHub-README in Chrome hart neu laden und das sichtbare Bild prüfen.

## Verwandte Dateien

- [README](../../../README.md)
- [Mitmachen](../../../CONTRIBUTING.md)
