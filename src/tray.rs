use crate::{
    autostart, backend, language::Language, live_status, notify, power_guard, settings,
    tray_history, weather_location, window_settings,
};
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
const ID_WEATHER_LOCATION: &str = "weather_location";
const ID_LIVE_STATUS_ON_START: &str = "live_status_on_start";
const ID_DEMO: &str = "demo";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const ID_LANGUAGE_DE: &str = "language_de";
const ID_LANGUAGE_EN: &str = "language_en";
const ID_SETTINGS: &str = "settings";
const ID_WINDOW_OPACITY_PREFIX: &str = "window_opacity_";
const ID_WINDOW_LEVEL_NORMAL: &str = "window_level_normal";
const ID_WINDOW_LEVEL_TOP: &str = "window_level_top";
const ID_WINDOW_LEVEL_BOTTOM: &str = "window_level_bottom";

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
    power_guard_active: bool,
    power_guard_error: bool,
    power_guard_last_attempted: Option<bool>,
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
    tray_history::start_session();
    if window_settings::live_status_on_start() {
        live_status::open_on_startup();
    }
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
        power_guard_active: false,
        power_guard_error: false,
        power_guard_last_attempted: None,
    };
    app.sync_power_guard();
    app.sync_expected_exit();
    let result = event_loop.run_app(&mut app);
    let _ = power_guard::set_prevent_sleep(false);
    if result.is_ok() {
        tray_history::finish_session();
    }
    result?;
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
                    self.sync_power_guard();
                    self.sync_expected_exit();
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
                                            tray_history::set_expected_exit(false);
                                            Some(format!("Fehler beim Bestätigen: {error}"))
                                        }
                                    }
                                }
                                notify::NoticeAction::Cancel => {
                                    match backend::stop("warning_dialog") {
                                        Ok(()) => {
                                            tray_history::set_expected_exit(false);
                                            Some("Nachtmodus abgebrochen".into())
                                        }
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
                    if !self.power_guard_error
                        && self
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
            "{:?}|{:?}|{}|{}|{:?}|{}",
            self.status,
            self.message,
            autostart::enabled(),
            window_settings::opacity(),
            window_settings::WindowLevel::current(),
            window_settings::live_status_on_start(),
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

    fn set_power_guard(&mut self, active: bool) -> Result<()> {
        if active == self.power_guard_active && !self.power_guard_error {
            return Ok(());
        }
        power_guard::set_prevent_sleep(active)?;
        self.power_guard_active = active;
        self.power_guard_last_attempted = Some(active);
        if self.power_guard_error {
            self.message = None;
        }
        self.power_guard_error = false;
        Ok(())
    }

    fn sync_power_guard(&mut self) {
        let should_prevent_sleep = matches!(
            self.status,
            backend::WatchStatus::Watching { .. } | backend::WatchStatus::ShutdownWarning { .. }
        );
        if self.power_guard_last_attempted == Some(should_prevent_sleep) {
            return;
        }
        self.power_guard_last_attempted = Some(should_prevent_sleep);
        if let Err(error) = self.set_power_guard(should_prevent_sleep) {
            self.power_guard_error = true;
            self.power_guard_last_attempted = None;
            self.message = Some(format!("Fehler: {error}"));
            if should_prevent_sleep {
                let stop_result = backend::stop("power_guard_failed");
                if let Err(stop_error) = stop_result {
                    self.message = Some(format!(
                        "Fehler: Windows-Energiesperre fehlgeschlagen ({error}); Nachtmodus konnte nicht sicher gestoppt werden ({stop_error})"
                    ));
                } else {
                    self.message = Some(format!(
                        "Fehler: Windows-Energiesperre fehlgeschlagen; Nachtmodus wurde sicher gestoppt ({error})"
                    ));
                }
            }
        }
    }

    fn sync_expected_exit(&self) {
        match self.status {
            backend::WatchStatus::ShutdownWarning {
                demo: false,
                observe_only: false,
                ..
            } => tray_history::set_expected_exit(true),
            backend::WatchStatus::Watching { .. } | backend::WatchStatus::Off { .. } => {
                tray_history::set_expected_exit(false)
            }
            // Keep the expected marker through the brief Finished state so a
            // completed automatic power action is not mistaken for a crash.
            backend::WatchStatus::Finished { .. }
            | backend::WatchStatus::ShutdownWarning { .. } => {}
        }
    }

    fn perform(&mut self, action: &str, event_loop: &ActiveEventLoop) {
        let result = match action {
            ID_START => backend::start(false),
            ID_OBSERVE => backend::start(true),
            ID_STOP => backend::stop("tray_menu"),
            ID_LOG => backend::open_log(),
            ID_LIVE_STATUS => live_status::open(),
            ID_WEATHER_LOCATION => weather_location::open(),
            ID_LIVE_STATUS_ON_START => {
                window_settings::set_live_status_on_start(!window_settings::live_status_on_start())
            }
            ID_DEMO => backend::demo(),
            ID_SETTINGS => settings::open(),
            ID_AUTOSTART => autostart::set_enabled(!autostart::enabled()),
            ID_LANGUAGE_DE => Language::German.set(),
            ID_LANGUAGE_EN => Language::English.set(),
            ID_WINDOW_LEVEL_NORMAL => window_settings::WindowLevel::Normal.set(),
            ID_WINDOW_LEVEL_TOP => window_settings::WindowLevel::AlwaysOnTop.set(),
            ID_WINDOW_LEVEL_BOTTOM => window_settings::WindowLevel::AlwaysOnBottom.set(),
            action if action.starts_with(ID_WINDOW_OPACITY_PREFIX) => action
                .trim_start_matches(ID_WINDOW_OPACITY_PREFIX)
                .parse::<u8>()
                .map_err(|error| anyhow::anyhow!("invalid opacity value: {error}"))
                .and_then(window_settings::set_opacity),
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
                tray_history::finish_session();
                event_loop.exit();
                return;
            }
            _ => return,
        };
        let succeeded = result.is_ok();
        self.message = match result {
            Ok(()) => Some(match action {
                ID_START => "Nachtmodus gestartet".into(),
                ID_OBSERVE => "Beobachtung gestartet - kein Shutdown".into(),
                ID_STOP => "Nachtmodus gestoppt".into(),
                ID_LOG => "Protokoll geöffnet".into(),
                ID_LIVE_STATUS => "Live-Status geöffnet".into(),
                ID_WEATHER_LOCATION => self
                    .language
                    .text("Wetterort geöffnet", "Weather location opened")
                    .into(),
                ID_LIVE_STATUS_ON_START => self
                    .language
                    .text(
                        "Live-Fenster-Start geändert",
                        "Live-window startup setting changed",
                    )
                    .into(),
                ID_DEMO => "Demo gestartet - kein Shutdown".into(),
                ID_SETTINGS => "Einrichtung geöffnet".into(),
                ID_AUTOSTART => "Autostart geändert".into(),
                ID_LANGUAGE_DE => "Sprache auf Deutsch gesetzt".into(),
                ID_LANGUAGE_EN => "Language set to English".into(),
                ID_WINDOW_LEVEL_NORMAL => self
                    .language
                    .text(
                        "Fenster auf normale Ebene gesetzt",
                        "Window set to normal level",
                    )
                    .into(),
                ID_WINDOW_LEVEL_TOP => self
                    .language
                    .text("Fenster bleibt im Vordergrund", "Window stays on top")
                    .into(),
                ID_WINDOW_LEVEL_BOTTOM => self
                    .language
                    .text(
                        "Fenster bleibt im Hintergrund",
                        "Window stays in background",
                    )
                    .into(),
                action if action.starts_with(ID_WINDOW_OPACITY_PREFIX) => {
                    let value = action.trim_start_matches(ID_WINDOW_OPACITY_PREFIX);
                    if self.language == Language::German {
                        format!("Fenstertransparenz auf {value} % gesetzt")
                    } else {
                        format!("Window opacity set to {value}%")
                    }
                }
                _ => String::new(),
            }),
            Err(error) => Some(format!("Fehler: {error}")),
        };
        if succeeded {
            match action {
                ID_START | ID_OBSERVE => {
                    if let Err(error) = self.set_power_guard(true) {
                        self.power_guard_error = true;
                        self.power_guard_last_attempted = None;
                        let rollback = backend::stop("power_guard_failed");
                        self.message = Some(match rollback {
                            Ok(()) => format!(
                                "Fehler: Windows-Energiesperre fehlgeschlagen; Nachtmodus wurde sicher gestoppt ({error})"
                            ),
                            Err(stop_error) => format!(
                                "Fehler: Windows-Energiesperre fehlgeschlagen ({error}); Nachtmodus konnte nicht sicher gestoppt werden ({stop_error})"
                            ),
                        });
                    }
                }
                ID_STOP => {
                    tray_history::set_expected_exit(false);
                    if let Err(error) = self.set_power_guard(false) {
                        self.power_guard_error = true;
                        self.power_guard_last_attempted = None;
                        self.message = Some(format!("Fehler: {error}"));
                    }
                }
                _ => {}
            }
        }
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
        ID_WEATHER_LOCATION,
        language.text("Wetterort ändern", "Change weather location"),
        true,
        None,
    ));
    let _ = menu.append(&CheckMenuItem::with_id(
        ID_LIVE_STATUS_ON_START,
        language.text(
            "Live-Fenster beim Start öffnen",
            "Open live window at startup",
        ),
        true,
        window_settings::live_status_on_start(),
        None,
    ));
    let _ = menu.append(&MenuItem::with_id(
        ID_SETTINGS,
        language.text("Einrichtung öffnen", "Open setup"),
        true,
        None,
    ));
    let window_submenu = Submenu::new(
        language.text("Fenstereinstellungen", "Window settings"),
        true,
    );
    let opacity_submenu = Submenu::new(language.text("Fenstertransparenz", "Window opacity"), true);
    let current_opacity = window_settings::opacity();
    for value in window_settings::OPACITY_VALUES {
        let id = format!("{ID_WINDOW_OPACITY_PREFIX}{value}");
        let label = if value == 100 {
            language
                .text("100 % (deckend)", "100% (opaque)")
                .to_string()
        } else {
            format!("{value} %")
        };
        let _ = opacity_submenu.append(&CheckMenuItem::with_id(
            id,
            label,
            true,
            value == current_opacity,
            None,
        ));
    }
    let _ = window_submenu.append(&opacity_submenu);
    let level_submenu = Submenu::new(language.text("Fensterebene", "Window level"), true);
    let current_level = window_settings::WindowLevel::current();
    let _ = level_submenu.append(&CheckMenuItem::with_id(
        ID_WINDOW_LEVEL_NORMAL,
        language.text("Normal", "Normal"),
        true,
        current_level == window_settings::WindowLevel::Normal,
        None,
    ));
    let _ = level_submenu.append(&CheckMenuItem::with_id(
        ID_WINDOW_LEVEL_TOP,
        language.text("Immer im Vordergrund", "Always on top"),
        true,
        current_level == window_settings::WindowLevel::AlwaysOnTop,
        None,
    ));
    let _ = level_submenu.append(&CheckMenuItem::with_id(
        ID_WINDOW_LEVEL_BOTTOM,
        language.text("Immer im Hintergrund", "Always in background"),
        true,
        current_level == window_settings::WindowLevel::AlwaysOnBottom,
        None,
    ));
    let _ = window_submenu.append(&level_submenu);
    let _ = menu.append(&window_submenu);
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
        backend::WatchStatus::Off { .. } => (96, 165, 250),
        backend::WatchStatus::Watching {
            observe_only: true, ..
        } => (59, 130, 246),
        backend::WatchStatus::Watching { quiet: true, .. } => (245, 158, 11),
        backend::WatchStatus::Watching { .. } => (34, 197, 94),
        backend::WatchStatus::ShutdownWarning { .. } => (248, 177, 110),
        backend::WatchStatus::Finished { outcome, .. } if outcome.contains("confirmed") => {
            (242, 145, 150)
        }
        backend::WatchStatus::Finished { .. } => (96, 165, 250),
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
