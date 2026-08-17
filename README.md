# Herdr Night Watch

A small Windows tray app, live Herdr dashboard, lightweight Windows system monitor, and media-session companion for the existing fail-closed Herdr night watcher.

The app controls the robust WSL background watcher; it does not replace it.

This repository contains the Rust tray app, the Python watcher, Windows scripts, tests, documentation, and a tested Windows executable.

## Why this tool exists

We often let Herdr run overnight because Compound Engineering plans can become very large. Once such a plan starts, several agents may continue working autonomously for hours. The computer does not need to stay on all night, though: when all work is complete, Windows should reliably enter sleep mode or shut down.

That handoff between “Herdr is finished” and “Windows may go to sleep” was not reliably solved before. Herdr Night Watch closes that gap: it monitors the agents fail-closed, makes the current state visible, and performs the selected power action only after a configurable warning period. The same compact window is also a useful “what is my computer doing right now?” surface: it shows the Herdr workload, CPU/RAM/GPU/VRAM and GPU power telemetry, weather for a chosen place, and the music that Windows currently considers active.

The interface supports Deutsch and English. Change the language from the tray right-click menu under “Sprache / Language”. The live window also provides a compact system monitor for CPU, RAM, GPU, occupied VRAM, and NVIDIA GPU power use.

## Agent support and setup

Herdr Night Watch is not tied to one coding agent. It works with Codex, Claude Code, or any other agent that Herdr can report. The only required integration is Herdr itself.

## How the pieces fit together

Codex and Claude Code do the work. Herdr is the shared terminal runtime that knows whether their agents are still working, waiting, or finished. Herdr Night Watch does not read prompts, source code, or terminal output. It only asks Herdr for the current agent states and uses that answer to decide whether Windows may sleep.

```text
Codex / Claude Code / other coding agents
                    |
                    v
        Herdr terminal runtime in WSL
                    |
                    v
  herdr-night-watch.py checks agent states
                    |
                    +-- work remains -> keep watching
                    +-- status uncertain -> do nothing (fail-closed)
                    +-- all work complete -> visible warning period
                                             |
                                             v
                                  Windows sleep or shutdown

Windows tray app = configuration, live status, start/stop, and cancellation
```

The tray app never launches, stops, or controls Codex or Claude Code. It only watches the state Herdr reports. This is why one Night Watch installation can support mixed Herdr sessions, for example Codex and Claude Code working in parallel.

Open **Open setup** from the tray right-click menu to set:

- the name of your WSL distribution, for example `Ubuntu`;
- the WSL path to `herdr-night-watch.py`.

For a checkout of this repository, the path normally looks like `/home/your-name/projects/herdr-night-watch/watcher/herdr-night-watch.py`. The setup is stored locally in the Windows user registry and is never committed to Git.

## Requirements

- Windows with WSL installed;
- a WSL distribution with Python 3 and the Herdr CLI available;
- Codex, Claude Code, or another agent running inside Herdr when there is work to monitor;
- the Herdr Night Watch tray app running while a night run is active.

## Live status

<p>
  <img src="https://raw.githubusercontent.com/simfor99/herdr-night-watch/main/docs/images/live-status-de-latest.png" alt="Herdr Night Watch live status in German" width="49%">
  <img src="https://raw.githubusercontent.com/simfor99/herdr-night-watch/main/docs/images/live-status-en-latest.png" alt="Herdr Night Watch live status in English" width="49%">
</p>

The live window is intentionally compact: Herdr counts and the night-mode controls remain in the main panel, while the equal-width footer turns it into a quick system monitor without affecting the watcher. CPU, RAM, GPU, VRAM utilization, and NVIDIA GPU power use a soft traffic-light palette: green for normal load, pastel yellow for medium load, and pastel red for high load. Missing hardware telemetry is shown as `—` rather than guessed. The small upper-right control hood opens the last 30 completion actions and cycles the window between normal, always-on-top, and always-in-background modes. Its glass surfaces use a subtle top reflection to keep the dashboard calm but tactile. The tray menu lets you choose window opacity from 100% down to 10%.

### Spotify, Tidal and other media players

When Windows exposes an active media session, the footer shows the current artist and title in compact color-coded pills. Spotify is a supported example, as are Tidal and other players that publish playback through Windows' Global System Media Transport Controls. Night Watch does not log in to Spotify, call the Spotify API, read playlists, or access account data. Windows provides the currently active media metadata, and Night Watch reads only that local session snapshot.

The dotted timeline gives an immediate playback overview. Hovering previews the position that would be selected; clicking seeks there when the media session allows playback-position changes. The played dots transition softly from the artist color toward the title color, while the unplayed track remains subdued. This makes the current song readable at a glance without adding a full music player, album art, volume controls, or playback buttons. The media strip is informational and independent of the Herdr watcher: music can continue while Herdr works, and Herdr monitoring continues when no media session exists.

The moon also shows the current temperature for the selected weather location. Leipzig is used initially; the small weather control at the lower-right appears on hover and opens a searchable city selector. Weather is informational only, is refreshed in the background, and falls back to the last value or `—` when the network is unavailable.

## Usage

- **Start night mode**: continuously monitors all agents currently reported by Herdr and only shuts down Windows after the configured quiet period.
- **Prevent idle sleep while watching**: while a night run is active, the tray app tells Windows that work is still required so an automatic idle-sleep timer cannot interrupt Herdr. The guard is released when the run stops or the app exits; the watcher's own confirmed sleep or shutdown action is still allowed.
- **Observe only**: runs the same monitoring flow without performing a shutdown.
- **Stop and cancel shutdown**: ends the run and cancels only a pending shutdown scheduled by the watcher.
- **Demo: simulate completion**: shows the quiet period and shutdown warning within a few seconds. It can never shut down Windows.
- **Open live status**: opens a freely movable status window that can be closed at any time. Left-clicking the tray icon opens it; right-clicking shows the menu.
- **Reliable live window**: opening the live status again restores and focuses the existing window instead of creating a duplicate. Its last desktop position is stored locally and reused after the next start.
- **Open live window at startup**: optionally reopen the live status window automatically whenever the tray app starts. The default remains tray-only until you enable this option.
- **Completion log**: the live window's upper-right log button opens a read-only view of the last 30 requested sleep and shutdown actions plus detected unclean tray exits. An unclean exit is recorded when the tray starts again without finding a clean session marker.
- **Weather location**: hover the small weather control at the lower-right of the live window to search and select a city or postal code. The selected location is stored locally in the Windows user registry.
- **Window settings**: choose opacity from 100% (opaque) to 10% in 10-point steps, cycle the live window between normal, always-on-top, and always-in-background placement, and choose whether the live window appears in the Windows taskbar. These settings are stored locally and can be changed while the live window is open; hiding the taskbar entry does not hide the desktop window.
- The live-status footer gives a compact, informational view of CPU, RAM, GPU, occupied VRAM, and an available GPU power reading. Pastel yellow and red indicate medium and high utilization. Unsupported values show `—` and never affect the watcher.
- **Start with Windows**: starts only the tray app when you log in. It does not automatically arm a night run.

For maintainers, `windows/Test-HerdrNightWatchPowerGuard.ps1` is a non-destructive Windows smoke test for this protection. Run it from an elevated PowerShell; it verifies activation and release with `powercfg /requests` and never sleeps or shuts down the computer.

If a live-window start ever fails, the tray reports the failure and records a
diagnostic line in the local `logs/ui-errors.log` file. This file is runtime
data and is never part of the repository.

Weather data uses [Open-Meteo's geocoding](https://open-meteo.com/en/docs/geocoding-api) and [forecast](https://open-meteo.com/en/docs) services. The location and temperature are never used by the safety-critical watcher.

### Licensing and weather service

The Herdr Night Watch source code is MIT-licensed. The weather feature is a
runtime service integration and has separate provider terms: Open-Meteo's free
endpoints are for open-source and non-commercial use, are rate-limited, and
require attribution. Forecast and geocoding data are offered under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
For commercial deployments, use an appropriate Open-Meteo commercial/customer
plan or replace the weather provider. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
for the exact notices and links. The core Herdr watcher remains usable under
the MIT license without the weather feature.

The Python watcher lives at `watcher/herdr-night-watch.py`; its safety contract remains authoritative.

## Installation

1. Set up WSL with a working Herdr CLI and an Ubuntu distribution.
2. Choose a WSL path for `watcher/herdr-night-watch.py`.
3. Start `dist/Herdr-Nachtwaechter.exe` and open **Open setup** from its tray menu.
4. Enter the WSL distribution and watcher path, then start a night mode from the tray app.

The EXE is a convenience artifact for Windows. For other architectures or after source changes, rebuild it from `src/`.

The tooltip and live status window show the current Herdr count for information only. If Herdr cannot be read, the app never invents a number and the shutdown decision remains fail-closed.

An armed night run never survives a Windows or WSL restart. The watcher compares a composite WSL and Windows boot marker, uses a 30-second cache for ordinary status display, and forces a fresh comparison at safety-critical transitions. A changed marker clears any stale warning, records the reset, and returns to the safe inactive state. A new night run must always be started deliberately.

In a real night run, five seconds of confirmed inactivity starts the 300-second warning period. The Windows dialog offers `Cancel`; choosing it stops the watcher and cancels that specific pending shutdown.

## Maintenance documentation

The complete technical documentation for future changes starts at [docs/INDEX.md](docs/INDEX.md). It covers states, the Windows/WSL boundary, builds and delivery, and safe troubleshooting. Runtime data, logs, and personal state files do not belong in Git.

## Icon

The tray icon is based on Google’s official Material `bedtime` icon and is licensed under Apache-2.0. Its color changes by state: gray, green, blue, yellow, or red.
