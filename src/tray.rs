use crate::{autostart, backend, language::Language, live_status, notify};
use anyhow::Result;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::WindowId;

const ID_START: &str = "start";
const ID_OBSERVE: &str = "observe";
const ID_STOP: &str = "stop";
const ID_LOG: &str = "log";
const ID_LIVE_STATUS: &str = "live_status";
const ID_DEMO: &str = "demo";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const ID_LANGUAGE_DE: &str = "language_de";
const ID_LANGUAGE_EN: &str = "language_en";

#[derive(Clone, Copy)]
struct Wake;

struct App {
    language: Language,
    tray: Option<TrayIcon>,
    status: backend::WatchStatus,
    message: Option<String>,
    last_view: String,
    refresh_at: Instant,
    checking: bool,
    result_rx: mpsc::Receiver<Result<backend::WatchStatus, String>>,
    result_tx: mpsc::Sender<Result<backend::WatchStatus, String>>,
}

pub fn run() -> Result<()> {
    let event_loop: EventLoop<Wake> = EventLoop::with_user_event().build()?;
    let proxy: EventLoopProxy<Wake> = event_loop.create_proxy();
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(1));
            let _ = proxy.send_event(Wake);
        }
    });

    let language = Language::current();
    let initial = backend::status().unwrap_or(backend::WatchStatus::Off {
        agents: backend::AgentSummary::default(),
        completion_action: backend::CompletionAction::Shutdown,
        warning_seconds: 300,
    });
    let icon = icon_for(&initial)?;
    let tray = TrayIconBuilder::new()
        .with_tooltip(tooltip(&initial, None, language))
        .with_icon(icon)
        .with_menu(Box::new(menu_for(
            &initial,
            None,
            autostart::enabled(),
            language,
        )))
        .with_menu_on_left_click(false)
        .build()?;
    let mut app = App {
        language,
        tray: Some(tray),
        status: initial,
        message: None,
        last_view: String::new(),
        refresh_at: Instant::now() - Duration::from_secs(10),
        checking: false,
        result_rx,
        result_tx,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

impl App {
    fn refresh_status(&mut self) {
        if self.checking || self.refresh_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.checking = true;
        self.refresh_at = Instant::now();
        let sender = self.result_tx.clone();
        thread::spawn(move || {
            let result = backend::status().map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn apply_results(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.checking = false;
            match result {
                Ok(status) => {
                    let show_notice =
                        matches!(status, backend::WatchStatus::ShutdownWarning { .. })
                            && !matches!(self.status, backend::WatchStatus::ShutdownWarning { .. });
                    self.status = status;
                    if show_notice {
                        let (
                            demo,
                            observe_only,
                            warning_seconds,
                            completion_action,
                            network_triggered,
                        ) = match self.status {
                            backend::WatchStatus::ShutdownWarning {
                                demo,
                                observe_only,
                                warning_seconds,
                                completion_action,
                                network_triggered,
                                ..
                            } => (
                                demo,
                                observe_only,
                                warning_seconds,
                                completion_action,
                                network_triggered,
                            ),
                            _ => unreachable!(),
                        };
                        if !demo && !observe_only {
                            self.message = match notify::completion_notice(
                                self.language,
                                demo,
                                warning_seconds,
                                completion_action,
                                network_triggered,
                            ) {
                                notify::NoticeAction::Confirm => {
                                    match backend::confirm_completion() {
                                        Ok(()) => Some("Abschluss sofort bestätigt".into()),
                                        Err(error) => {
                                            Some(format!("Fehler beim Bestätigen: {error}"))
                                        }
                                    }
                                }
                                notify::NoticeAction::Cancel => {
                                    match backend::stop("warning_dialog") {
                                        Ok(()) => Some("Nachtmodus abgebrochen".into()),
                                        Err(error) => {
                                            Some(format!("Fehler beim Abbrechen: {error}"))
                                        }
                                    }
                                }
                            };
                            self.refresh_at = Instant::now() - Duration::from_secs(10);
                        } else if demo {
                            let _ = notify::completion_notice(
                                self.language,
                                demo,
                                warning_seconds,
                                completion_action,
                                network_triggered,
                            );
                        }
                    }
                    if self
                        .message
                        .as_deref()
                        .is_some_and(|message| message.starts_with("Fehler:"))
                    {
                        self.message = None;
                    }
                }
                Err(error) => self.message = Some(format!("Fehler: {error}")),
            }
        }
    }

    fn render(&mut self) {
        let view = format!(
            "{:?}|{:?}|{}",
            self.status,
            self.message,
            autostart::enabled()
        );
        if view == self.last_view {
            return;
        }
        self.last_view = view;
        if let Some(tray) = &self.tray {
            let _ = tray.set_tooltip(Some(tooltip(
                &self.status,
                self.message.as_deref(),
                self.language,
            )));
            if let Ok(icon) = icon_for(&self.status) {
                let _ = tray.set_icon(Some(icon));
            }
            tray.set_menu(Some(Box::new(menu_for(
                &self.status,
                self.message.as_deref(),
                autostart::enabled(),
                self.language,
            ))));
        }
    }

    fn perform(&mut self, action: &str, event_loop: &ActiveEventLoop) {
        let result = match action {
            ID_START => backend::start(false),
            ID_OBSERVE => backend::start(true),
            ID_STOP => backend::stop("tray_menu"),
            ID_LOG => backend::open_log(),
            ID_LIVE_STATUS => live_status::open(),
            ID_DEMO => backend::demo(),
            ID_AUTOSTART => autostart::set_enabled(!autostart::enabled()),
            ID_LANGUAGE_DE => Language::German.set(),
            ID_LANGUAGE_EN => Language::English.set(),
            ID_QUIT => {
                if matches!(
                    self.status,
                    backend::WatchStatus::Watching { .. }
                        | backend::WatchStatus::ShutdownWarning { .. }
                ) {
                    // The tray owns the guaranteed foreground warning. Do not leave a
                    // running night watch behind when its only warning surface exits.
                    let _ = backend::stop("tray_app_quit");
                }
                event_loop.exit();
                return;
            }
            _ => return,
        };
        self.message = match result {
            Ok(()) => Some(match action {
                ID_START => "Nachtmodus gestartet".into(),
                ID_OBSERVE => "Beobachtung gestartet - kein Shutdown".into(),
                ID_STOP => "Nachtmodus gestoppt".into(),
                ID_LOG => "Protokoll geöffnet".into(),
                ID_LIVE_STATUS => "Live-Status geöffnet".into(),
                ID_DEMO => "Demo gestartet - kein Shutdown".into(),
                ID_AUTOSTART => "Autostart geändert".into(),
                ID_LANGUAGE_DE => "Sprache auf Deutsch gesetzt".into(),
                ID_LANGUAGE_EN => "Language set to English".into(),
                _ => String::new(),
            }),
            Err(error) => Some(format!("Fehler: {error}")),
        };
        if action == ID_LANGUAGE_DE {
            self.language = Language::German;
        }
        if action == ID_LANGUAGE_EN {
            self.language = Language::English;
        }
        self.refresh_at = Instant::now() - Duration::from_secs(10);
    }

    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        self.apply_results();
        self.refresh_status();
        self.render();
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                self.perform(ID_LIVE_STATUS, event_loop);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.perform(event.id.as_ref(), event_loop);
        }
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Wake) {
        self.tick(event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.tick(event_loop);
    }
}

fn menu_for(
    status: &backend::WatchStatus,
    message: Option<&str>,
    autostart: bool,
    language: Language,
) -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id(
        "status",
        status_line(status, language),
        false,
        None,
    ));
    if let Some(message) = message {
        let _ = menu.append(&MenuItem::with_id("message", message, false, None));
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let active = matches!(
        status,
        backend::WatchStatus::Watching { .. } | backend::WatchStatus::ShutdownWarning { .. }
    );
    let _ = menu.append(&MenuItem::with_id(
        ID_START,
        language.text("Nachtmodus starten", "Start night mode"),
        !active,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ID_OBSERVE,
        language.text("Nur beobachten", "Observe only"),
        !active,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ID_STOP,
        language.text("Stopp und Shutdown abbrechen", "Stop and cancel shutdown"),
        active,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ID_DEMO,
        language.text("Demo: Abschluss simulieren", "Demo: simulate completion"),
        !active,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        ID_LIVE_STATUS,
        language.text("Live-Status öffnen", "Open live status"),
        true,
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ID_LOG,
        language.text("Protokoll öffnen", "Open log"),
        true,
        None,
    ));
    let _ = menu.append(&CheckMenuItem::with_id(
        ID_AUTOSTART,
        language.text("Mit Windows starten", "Start with Windows"),
        true,
        autostart,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let languages = Submenu::new("Sprache / Language", true);
    let _ = languages.append(&CheckMenuItem::with_id(
        ID_LANGUAGE_DE,
        "Deutsch",
        true,
        language == Language::German,
        None,
    ));
    let _ = languages.append(&CheckMenuItem::with_id(
        ID_LANGUAGE_EN,
        "English",
        true,
        language == Language::English,
        None,
    ));
    let _ = menu.append(&languages);
    let _ = menu.append(&MenuItem::with_id(
        ID_QUIT,
        language.text("Tray-App beenden", "Quit tray app"),
        true,
        None,
    ));
    menu
}

fn status_line(status: &backend::WatchStatus, language: Language) -> String {
    match status {
        backend::WatchStatus::Off { agents, .. } => {
            format!(
                "● {} - {}",
                language.text("Aus - kein Nachtlauf aktiv", "Off - no night run active"),
                agent_summary(agents, language)
            )
        }
        backend::WatchStatus::Watching {
            targets,
            live_scope,
            observe_only,
            quiet,
            demo,
            agents,
            ..
        } => {
            if *demo {
                return format!(
                    "● Demo: Ruhezeit und Warnung werden simuliert - {}",
                    agent_summary(agents, language)
                );
            }
            let mode = if *observe_only {
                "Beobachtung"
            } else {
                "Nachtmodus"
            };
            let phase = if *quiet {
                "Ruhezeit läuft"
            } else {
                "Agenten arbeiten oder warten"
            };
            let scope = if *live_scope {
                "alle aktuellen Herdr-Agenten".to_string()
            } else {
                format!("{targets} überwacht")
            };
            format!(
                "● {mode}: {scope} - {phase} - {}",
                agent_summary(agents, language)
            )
        }
        backend::WatchStatus::ShutdownWarning {
            targets,
            live_scope,
            demo,
            agents,
            ..
        } => {
            if *demo {
                format!(
                    "● Demo-Warnung: kein Windows-Shutdown - {}",
                    agent_summary(agents, language)
                )
            } else {
                let scope = if *live_scope {
                    "alle aktuellen Herdr-Agenten".to_string()
                } else {
                    format!("{targets} überwacht")
                };
                format!(
                    "● Shutdown-Warnfrist: {scope} - Stopp bricht ab - {}",
                    agent_summary(agents, language)
                )
            }
        }
        backend::WatchStatus::Finished {
            outcome, agents, ..
        } => {
            format!(
                "● Letzter Lauf: {outcome} - {}",
                agent_summary(agents, language)
            )
        }
    }
}

fn agent_summary(agents: &backend::AgentSummary, language: Language) -> String {
    if !agents.available {
        return language
            .text("Herdr nicht erreichbar", "Herdr unavailable")
            .into();
    }
    if language == Language::English {
        format!(
            "Herdr: {} detected, {} working",
            agents.total, agents.working
        )
    } else {
        format!(
            "Herdr: {} erkannt, {} arbeitet",
            agents.total, agents.working
        )
    }
}

fn tooltip(status: &backend::WatchStatus, _message: Option<&str>, language: Language) -> String {
    format!(
        "{}\n{}",
        language.text("Herdr-Nachtwächter", "Herdr Night Watch"),
        compact_status_line(status, language)
    )
}

fn compact_status_line(status: &backend::WatchStatus, _language: Language) -> String {
    let agents = match status {
        backend::WatchStatus::Off { agents, .. }
        | backend::WatchStatus::Watching { agents, .. }
        | backend::WatchStatus::ShutdownWarning { agents, .. }
        | backend::WatchStatus::Finished { agents, .. } => agents,
    };
    let herdr = if agents.available {
        format!("Herdr {}/{} aktiv", agents.working, agents.total)
    } else {
        "Herdr nicht erreichbar".into()
    };
    match status {
        backend::WatchStatus::Off { .. } => format!("Aus · {herdr}"),
        backend::WatchStatus::Watching {
            targets,
            live_scope,
            observe_only,
            quiet,
            demo,
            ..
        } if *demo => format!("Demo · {herdr}"),
        backend::WatchStatus::Watching {
            targets,
            live_scope,
            observe_only,
            quiet,
            ..
        } => {
            let mode = if *observe_only {
                "Beobachtung"
            } else if *quiet {
                "Ruhezeit"
            } else {
                "Nacht"
            };
            let scope = if *live_scope {
                "alle Herdr-Agenten"
            } else {
                return format!("{mode} · {targets} Ziel · {herdr}");
            };
            format!("{mode} · {scope} · {herdr}")
        }
        backend::WatchStatus::ShutdownWarning {
            targets,
            live_scope,
            demo,
            ..
        } => {
            if *demo {
                format!("Demo-Warnung · {herdr}")
            } else if *live_scope {
                format!("Warnung · alle Herdr-Agenten · {herdr}")
            } else {
                format!("Warnung · {targets} Ziel · {herdr}")
            }
        }
        backend::WatchStatus::Finished { .. } => format!("Fertig · {herdr}"),
    }
}

fn icon_for(status: &backend::WatchStatus) -> Result<Icon> {
    let (red, green, blue) = match status {
        backend::WatchStatus::Off { .. } | backend::WatchStatus::Finished { .. } => (105, 112, 124),
        backend::WatchStatus::Watching {
            observe_only: true, ..
        } => (59, 130, 246),
        backend::WatchStatus::Watching { quiet: true, .. } => (245, 158, 11),
        backend::WatchStatus::Watching { .. } => (34, 197, 94),
        backend::WatchStatus::ShutdownWarning { .. } => (239, 68, 68),
    };
    let bytes = include_bytes!("../assets/material-bedtime-32.png");
    let decoder = png::Decoder::new(bytes.as_slice());
    let mut reader = decoder.read_info()?;
    let (width, height) = reader.info().size();
    let mut rgba = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut rgba)?;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = red;
        pixel[1] = green;
        pixel[2] = blue;
    }
    Ok(Icon::from_rgba(rgba, width, height)?)
}
