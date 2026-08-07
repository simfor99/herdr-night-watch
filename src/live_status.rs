use crate::{
    backend::{self, AgentSummary, CompletionAction, WatchStatus},
    language::Language,
    log_viewer,
    system_metrics::{self, SystemMetrics},
    window_settings,
};
use anyhow::{Context, Result};
use eframe::egui;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetWindowLongW, LWA_ALPHA, SetLayeredWindowAttributes,
    SetWindowLongW, WS_EX_LAYERED,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);
const ACCENT_STRONG: egui::Color32 = egui::Color32::from_rgb(59, 130, 246);
const GREEN: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(251, 191, 36);
const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);
const PASTEL_GREEN: egui::Color32 = egui::Color32::from_rgb(125, 220, 170);
const PASTEL_YELLOW: egui::Color32 = egui::Color32::from_rgb(245, 210, 125);
const PASTEL_RED: egui::Color32 = egui::Color32::from_rgb(242, 145, 150);
const PASTEL_ORANGE: egui::Color32 = egui::Color32::from_rgb(248, 177, 110);
const GRAY: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 232, 240);
const BG_TOP: egui::Color32 = egui::Color32::from_rgb(26, 34, 54);
const BG_BOTTOM: egui::Color32 = egui::Color32::from_rgb(14, 19, 33);
const WINDOW_DRAG_THRESHOLD_SQUARED: f32 = 4.0;

pub fn open() -> Result<()> {
    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    let mut command = Command::new(executable);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--live-status")
        .spawn()
        .context("Live-Status-Fenster konnte nicht gestartet werden")?;
    Ok(())
}

pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 170.0])
            .with_min_inner_size([390.0, 160.0])
            .with_decorations(false)
            .with_window_level(window_level(window_settings::WindowLevel::current()))
            .with_title(match Language::current() {
                Language::German => "Herdr-Nachtwächter - Live-Status",
                Language::English => "Herdr Night Watch - Live Status",
            }),
        ..Default::default()
    };
    eframe::run_native(
        match Language::current() {
            Language::German => "Herdr-Nachtwächter - Live-Status",
            Language::English => "Herdr Night Watch - Live Status",
        },
        options,
        Box::new(|creation_context| {
            configure_visuals(&creation_context.egui_ctx);
            Ok(Box::new(LiveStatusApp::new()))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Live-Status-Fenster konnte nicht ausgeführt werden: {error}"))
}

fn live_title(language: Language) -> &'static str {
    language.text(
        "Herdr-Nachtwächter - Live-Status",
        "Herdr Night Watch - Live Status",
    )
}

fn window_level(level: window_settings::WindowLevel) -> egui::WindowLevel {
    match level {
        window_settings::WindowLevel::Normal => egui::WindowLevel::Normal,
        window_settings::WindowLevel::AlwaysOnTop => egui::WindowLevel::AlwaysOnTop,
        window_settings::WindowLevel::AlwaysOnBottom => egui::WindowLevel::AlwaysOnBottom,
    }
}

fn apply_window_opacity(opacity: u8, title: &str) {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd.is_null() {
            return;
        }
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as i32);
        let alpha = ((u16::from(opacity) * 255 + 50) / 100) as u8;
        let _ = SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA);
    }
}

struct LiveStatusApp {
    language: Language,
    status: WatchStatus,
    status_rx: Receiver<Result<WatchStatus, String>>,
    status_tx: Sender<Result<WatchStatus, String>>,
    action_rx: Receiver<Result<NightAction, String>>,
    action_tx: Sender<Result<NightAction, String>>,
    last_refresh: Instant,
    checking: bool,
    action_in_progress: bool,
    pending_action: Option<NightAction>,
    error: Option<String>,
    toast: Option<Toast>,
    warning_seconds_input: String,
    editing_warning_seconds: bool,
    metrics_rx: Receiver<SystemMetrics>,
    metrics: SystemMetrics,
    opacity: Option<u8>,
    window_level: window_settings::WindowLevel,
    window_drag_started: bool,
}

impl LiveStatusApp {
    fn new() -> Self {
        let (status_tx, status_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (metrics_tx, metrics_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut sampler = system_metrics::Sampler::new();
            loop {
                let _ = metrics_tx.send(sampler.sample());
                thread::sleep(Duration::from_secs(2));
            }
        });
        Self {
            language: Language::current(),
            status: WatchStatus::Off {
                agents: AgentSummary::default(),
                completion_action: CompletionAction::Shutdown,
                warning_seconds: 300,
            },
            status_rx,
            status_tx,
            action_rx,
            action_tx,
            last_refresh: Instant::now() - Duration::from_secs(10),
            checking: false,
            action_in_progress: false,
            pending_action: None,
            error: None,
            toast: None,
            warning_seconds_input: "300".into(),
            editing_warning_seconds: false,
            metrics_rx,
            metrics: SystemMetrics::default(),
            opacity: None,
            window_level: window_settings::WindowLevel::current(),
            window_drag_started: false,
        }
    }

    fn refresh(&mut self) {
        if self.checking || self.last_refresh.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.checking = true;
        self.last_refresh = Instant::now();
        let sender = self.status_tx.clone();
        thread::spawn(move || {
            let _ = sender.send(backend::status().map_err(|error| error.to_string()));
        });
    }

    fn collect_metrics(&mut self) {
        while let Ok(metrics) = self.metrics_rx.try_recv() {
            self.metrics = metrics;
        }
    }

    fn collect_results(&mut self) {
        while let Ok(result) = self.status_rx.try_recv() {
            self.checking = false;
            match result {
                Ok(status) => {
                    let warning_seconds = backend::warning_seconds(&status);
                    self.status = status;
                    self.error = None;
                    if !self.editing_warning_seconds
                        && !matches!(self.pending_action, Some(NightAction::SetWarningSeconds(_)))
                    {
                        self.warning_seconds_input = warning_seconds.to_string();
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn run_action(&mut self, action: NightAction) {
        if self.action_in_progress {
            return;
        }
        self.action_in_progress = true;
        self.pending_action = Some(action);
        self.error = None;
        let sender = self.action_tx.clone();
        thread::spawn(move || {
            let result = match action {
                NightAction::Start => backend::start(false),
                NightAction::Stop => backend::stop("live_window_moon"),
                NightAction::SetCompletionAction(completion_action) => {
                    backend::set_completion_action(completion_action)
                }
                NightAction::SetWarningSeconds(seconds) => backend::set_warning_seconds(seconds),
            };
            let _ = sender.send(result.map(|()| action).map_err(|error| error.to_string()));
        });
    }

    fn collect_actions(&mut self) {
        while let Ok(result) = self.action_rx.try_recv() {
            self.action_in_progress = false;
            self.pending_action = None;
            match result {
                Ok(action) => {
                    let (message, color) = match action {
                        NightAction::Start => (
                            self.language
                                .text("Nachtmodus aktiv", "Night mode active")
                                .into(),
                            GREEN,
                        ),
                        NightAction::Stop => (
                            self.language
                                .text("Nachtmodus deaktiviert", "Night mode disabled")
                                .into(),
                            GRAY,
                        ),
                        NightAction::SetCompletionAction(CompletionAction::Sleep) => (
                            self.language
                                .text("Energiesparmodus gewählt", "Sleep selected")
                                .into(),
                            GREEN,
                        ),
                        NightAction::SetCompletionAction(CompletionAction::Shutdown) => (
                            self.language
                                .text("Herunterfahren gewählt", "Shutdown selected")
                                .into(),
                            RED,
                        ),
                        NightAction::SetWarningSeconds(seconds) => {
                            self.warning_seconds_input = seconds.to_string();
                            self.editing_warning_seconds = false;
                            (
                                format!(
                                    "{} {seconds} {}",
                                    self.language.text("Warnfrist auf", "Warning period set to"),
                                    self.language.text("Sekunden gesetzt", "seconds"),
                                ),
                                ACCENT,
                            )
                        }
                    };
                    self.toast = Some(Toast {
                        message,
                        color,
                        expires_at: Instant::now() + Duration::from_secs(3),
                    });
                    self.last_refresh = Instant::now() - Duration::from_secs(10);
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn show_toast(&mut self, context: &egui::Context) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| Instant::now() >= toast.expires_at)
        {
            self.toast = None;
        }
        if let Some(toast) = &self.toast {
            egui::Area::new(egui::Id::new("night-mode-toast"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -12.0))
                .interactable(false)
                .show(context, |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_unmultiplied(15, 23, 42, 238))
                        .stroke(egui::Stroke::new(1.0, toast.color))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(&toast.message).strong().color(TEXT));
                        });
                });
        }
    }
}

#[derive(Clone, Copy)]
enum NightAction {
    Start,
    Stop,
    SetCompletionAction(CompletionAction),
    SetWarningSeconds(u64),
}

struct Toast {
    message: String,
    color: egui::Color32,
    expires_at: Instant,
}

impl eframe::App for LiveStatusApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let current_language = Language::current();
        if current_language != self.language {
            self.language = current_language;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(
                live_title(current_language).into(),
            ));
        }
        let current_opacity = window_settings::opacity();
        if self.opacity != Some(current_opacity) {
            apply_window_opacity(current_opacity, live_title(self.language));
            self.opacity = Some(current_opacity);
        }
        let current_window_level = window_settings::WindowLevel::current();
        if current_window_level != self.window_level {
            self.window_level = current_window_level;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::WindowLevel(window_level(
                    current_window_level,
                )));
        }
        self.collect_results();
        self.collect_actions();
        self.collect_metrics();
        self.refresh();
        paint_gradient(ui.painter(), ui.max_rect(), BG_TOP, BG_BOTTOM);
        let (log_clicked, level_clicked, minimize_clicked, close_clicked) =
            window_controls(ui, self.language, self.window_level);
        if log_clicked {
            match log_viewer::open() {
                Ok(()) => {
                    self.toast = Some(Toast {
                        message: self
                            .language
                            .text("Abschlussprotokoll geöffnet", "Completion log opened")
                            .into(),
                        color: ACCENT,
                        expires_at: Instant::now() + Duration::from_secs(3),
                    });
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if level_clicked {
            let next_level = self.window_level.next();
            match next_level.set() {
                Ok(()) => {
                    self.window_level = next_level;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::WindowLevel(window_level(
                            next_level,
                        )));
                    self.toast = Some(Toast {
                        message: window_level_message(next_level, self.language).into(),
                        color: ACCENT,
                        expires_at: Instant::now() + Duration::from_secs(3),
                    });
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if minimize_clicked {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if close_clicked {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(14, 14))
            .show(ui, |ui| {
                let moon = moon_view(
                    &self.status,
                    self.pending_action,
                    self.error.as_deref(),
                    self.language,
                );
                let action = action_for(&self.status);
                let night_mode_active = night_mode_active(&self.status);
                let display_completion_action = completion_action_for_display(&self.status, self.pending_action);
                let countdown_seconds = countdown_seconds(&self.status);
                let gradient_rect = ui.max_rect();
                let row_size = egui::vec2(ui.available_width(), 88.0);
                ui.allocate_ui_with_layout(
                    row_size,
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.vertical(|ui| {
                            let mut switch_clicked = false;
                            let mut warning_seconds_to_save = None;
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(self.language.text("Herdr jetzt", "Herdr now")).strong().color(ACCENT));
                                ui.add_space(8.0);
                                let switch_tooltip = if night_mode_active {
                                    self.language.text(
                                        "Der Abschlussmodus ist für den laufenden Nachtmodus gespeichert.\nStopp den Nachtmodus, um ihn zu ändern.",
                                        "The completion mode is saved for the active night run.\nStop the night run to change it.",
                                    )
                                } else if display_completion_action == CompletionAction::Sleep {
                                    self.language.text(
                                        "Energiesparmodus nach Abschluss\nKlicken für Herunterfahren.",
                                        "Sleep after completion\nClick for shutdown.",
                                    )
                                } else {
                                    self.language.text(
                                        "Herunterfahren nach Abschluss\nKlicken für Energiesparmodus.",
                                        "Shutdown after completion\nClick for sleep.",
                                    )
                                };
                                let response = completion_switch(
                                    ui,
                                    display_completion_action,
                                    !night_mode_active && !self.action_in_progress,
                                )
                                .on_hover_text(switch_tooltip);
                                if response.hovered() && !night_mode_active && !self.action_in_progress {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                switch_clicked = response.clicked();
                                ui.add_space(7.0);
                                let seconds_response = ui.add_enabled(
                                    !night_mode_active && !self.action_in_progress,
                                    egui::TextEdit::singleline(&mut self.warning_seconds_input)
                                        .desired_width(31.0)
                                        .horizontal_align(egui::Align::Center),
                                );
                                let seconds_response = seconds_response.on_hover_text(
                                    if night_mode_active {
                                        self.language.text(
                                            "Die Warnfrist ist für den laufenden Nachtmodus gespeichert.",
                                            "The warning period is saved for the active night run.",
                                        )
                                    } else {
                                        self.language.text(
                                            "Warnfrist in Sekunden für den nächsten Nachtmodus.\nErlaubt sind 10 bis 3.600 Sekunden.",
                                            "Warning period in seconds for the next night run.\nAllowed: 10 to 3,600 seconds.",
                                        )
                                    },
                                );
                                if seconds_response.has_focus() {
                                    self.editing_warning_seconds = true;
                                }
                                if seconds_response.lost_focus() && self.editing_warning_seconds {
                                    self.editing_warning_seconds = false;
                                    match self.warning_seconds_input.trim().parse::<u64>() {
                                        Ok(seconds) if (10..=3600).contains(&seconds) => {
                                            warning_seconds_to_save = Some(seconds);
                                        }
                                        _ => {
                                            self.error = Some(
                                                self.language
                                                    .text(
                                                        "Die Warnfrist muss zwischen 10 und 3.600 Sekunden liegen.",
                                                        "The warning period must be between 10 and 3,600 seconds.",
                                                    )
                                                    .into(),
                                            );
                                            self.warning_seconds_input =
                                                backend::warning_seconds(&self.status).to_string();
                                        }
                                    }
                                }
                                ui.label(egui::RichText::new("s").small().color(GRAY));
                            });
                            if switch_clicked && !night_mode_active && !self.action_in_progress {
                                self.run_action(NightAction::SetCompletionAction(match display_completion_action {
                                    CompletionAction::Sleep => CompletionAction::Shutdown,
                                    CompletionAction::Shutdown => CompletionAction::Sleep,
                                }));
                            }
                            if let Some(seconds) = warning_seconds_to_save {
                                self.run_action(NightAction::SetWarningSeconds(seconds));
                            }
                            ui.add_space(5.0);
                            let panel = glassy_frame(ui).show(ui, |ui| {
                                let agents = agents_for(&self.status);
                                if agents.available {
                                    ui.horizontal(|ui| {
                                        metric(ui, self.language.text("Erkannt", "Detected"), agents.total, TEXT);
                                        divider(ui);
                                        metric(ui, self.language.text("Arbeitet", "Working"), agents.working, GREEN);
                                        divider(ui);
                                        metric(ui, self.language.text("Bereit", "Ready"), agents.idle, ACCENT);
                                        divider(ui);
                                        metric(ui, self.language.text("Fertig", "Finished"), agents.done, GRAY);
                                    });
                                } else {
                                    ui.colored_label(
                                        YELLOW,
                                        self.language.text(
                                            "Herdr ist gerade nicht erreichbar",
                                            "Herdr is currently unreachable",
                                        ),
                                    );
                                    ui.label(
                                        egui::RichText::new(self.language.text(
                                            "Es wird keine Zahl geschätzt.",
                                            "No number will be guessed.",
                                        ))
                                            .color(GRAY),
                                    );
                                }
                            });
                            glass_sheen(ui.painter(), panel.response.rect);
                        });
                        ui.add_space(14.0);
                        ui.vertical(|ui| {
                            ui.add_space(30.0);
                            let response = moon_icon(ui, moon.color, 59.5, gradient_rect)
                                .on_hover_text(moon.tooltip);
                            if response.hovered() && !self.action_in_progress {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if !self.action_in_progress && response.clicked() {
                                self.run_action(action);
                            }
                            if let Some(seconds_remaining) = countdown_seconds {
                                ui.add_space(1.0);
                                ui.label(
                                    egui::RichText::new(format_countdown(seconds_remaining))
                                        .strong()
                                        .color(RED),
                                );
                            }
                        });
                    },
                );
                ui.add_space(6.0);
                system_metrics_row(ui, self.metrics, self.language);
        });
        self.show_toast(ui.ctx());
        handle_window_drag(self, ui);
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn handle_window_drag(app: &mut LiveStatusApp, ui: &egui::Ui) {
    let rect = ui.max_rect();
    let completion_switch = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 86.0, rect.top() + 12.0),
        egui::pos2(rect.left() + 168.0, rect.top() + 50.0),
    );
    let warning_seconds = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 176.0, rect.top() + 12.0),
        egui::pos2(rect.left() + 244.0, rect.top() + 50.0),
    );
    let moon = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 104.0, rect.top() + 40.0),
        egui::pos2(rect.right() - 38.0, rect.top() + 106.0),
    );
    let control_hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 138.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 3.0, rect.top() + 30.0),
    );
    let is_excluded = |position: egui::Pos2| {
        completion_switch.contains(position)
            || warning_seconds.contains(position)
            || moon.contains(position)
            || control_hood.contains(position)
    };
    let (origin, position, total_delta, primary_down) = ui.input(|input| {
        (
            input.pointer.press_origin(),
            input.pointer.latest_pos(),
            input.pointer.total_drag_delta(),
            input.pointer.primary_down(),
        )
    });
    if !primary_down {
        app.window_drag_started = false;
    }
    let excluded_origin = origin.is_some_and(is_excluded);
    if primary_down
        && !app.window_drag_started
        && !excluded_origin
        && total_delta.is_some_and(|delta| delta.length_sq() > WINDOW_DRAG_THRESHOLD_SQUARED)
    {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        app.window_drag_started = true;
    }
    if let Some(position) = position
        && !is_excluded(position)
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
    }
}

fn window_controls(
    ui: &mut egui::Ui,
    language: Language,
    level: window_settings::WindowLevel,
) -> (bool, bool, bool, bool) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let button_width = 30.0;
    let gap = 1.0;
    let hood_width = button_width * 4.0 + gap * 3.0 + 10.0;
    let hood_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - hood_width, rect.top() + 2.0),
        egui::pos2(rect.right() - 4.0, rect.top() + 28.0),
    );
    let visible = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|position| hood_rect.contains(position));
    if !visible {
        return (false, false, false, false);
    }
    painter.rect_filled(
        hood_rect,
        egui::CornerRadius::same(10),
        egui::Color32::from_rgba_unmultiplied(28, 29, 38, 220),
    );
    painter.rect_stroke(
        hood_rect,
        egui::CornerRadius::same(10),
        egui::Stroke::new(1.0, egui::Color32::from_rgb(66, 74, 98)),
        egui::StrokeKind::Inside,
    );
    glass_sheen(ui.painter(), hood_rect);
    let log_rect = egui::Rect::from_min_max(
        egui::pos2(hood_rect.left() + 2.0, hood_rect.top()),
        egui::pos2(hood_rect.left() + button_width + 2.0, hood_rect.bottom()),
    );
    let level_rect = egui::Rect::from_min_max(
        egui::pos2(log_rect.right() + gap, hood_rect.top()),
        egui::pos2(log_rect.right() + gap + button_width, hood_rect.bottom()),
    );
    let close_rect = egui::Rect::from_min_max(
        egui::pos2(hood_rect.right() - button_width - 2.0, hood_rect.top()),
        egui::pos2(hood_rect.right() - 2.0, hood_rect.bottom()),
    );
    let minimize_rect = egui::Rect::from_min_max(
        egui::pos2(close_rect.left() - gap - button_width, hood_rect.top()),
        egui::pos2(close_rect.left() - gap, hood_rect.bottom()),
    );
    let log_response = ui
        .interact(
            log_rect,
            ui.make_persistent_id("live_window_log"),
            egui::Sense::click(),
        )
        .on_hover_text(language.text(
            "Letzte 30 Abschlussaktionen öffnen",
            "Open the last 30 completion actions",
        ));
    let level_response = ui
        .interact(
            level_rect,
            ui.make_persistent_id("live_window_level"),
            egui::Sense::click(),
        )
        .on_hover_text(window_level_tooltip(level, language));
    let minimize_response = ui.interact(
        minimize_rect,
        ui.make_persistent_id("live_window_minimize"),
        egui::Sense::click(),
    );
    let close_response = ui.interact(
        close_rect,
        ui.make_persistent_id("live_window_close"),
        egui::Sense::click(),
    );
    let minimize_fill = if minimize_response.hovered() {
        egui::Color32::from_rgb(52, 54, 66)
    } else {
        egui::Color32::TRANSPARENT
    };
    let close_fill = if close_response.hovered() {
        egui::Color32::from_rgb(190, 65, 78)
    } else {
        egui::Color32::TRANSPARENT
    };
    let level_fill = if level_response.hovered() {
        egui::Color32::from_rgb(52, 54, 66)
    } else {
        egui::Color32::TRANSPARENT
    };
    let log_fill = if log_response.hovered() {
        egui::Color32::from_rgb(52, 54, 66)
    } else {
        egui::Color32::TRANSPARENT
    };
    painter.rect_filled(log_rect, egui::CornerRadius::same(8), log_fill);
    painter.rect_filled(level_rect, egui::CornerRadius::same(8), level_fill);
    painter.rect_filled(minimize_rect, egui::CornerRadius::same(8), minimize_fill);
    painter.rect_filled(close_rect, egui::CornerRadius::same(8), close_fill);
    let icon_color = if close_response.hovered() {
        egui::Color32::WHITE
    } else {
        GRAY
    };
    draw_log_icon(
        painter,
        log_rect.center(),
        if log_response.hovered() { ACCENT } else { GRAY },
    );
    draw_window_level_icon(
        painter,
        level_rect.center(),
        level,
        if level_response.hovered() {
            level_color(level)
        } else {
            GRAY
        },
    );
    painter.line_segment(
        [
            egui::pos2(minimize_rect.left() + 11.0, minimize_rect.center().y + 4.0),
            egui::pos2(minimize_rect.right() - 11.0, minimize_rect.center().y + 4.0),
        ],
        egui::Stroke::new(1.2, icon_color),
    );
    painter.line_segment(
        [
            egui::pos2(close_rect.left() + 11.0, close_rect.top() + 10.0),
            egui::pos2(close_rect.right() - 11.0, close_rect.bottom() - 10.0),
        ],
        egui::Stroke::new(1.2, icon_color),
    );
    painter.line_segment(
        [
            egui::pos2(close_rect.right() - 11.0, close_rect.top() + 10.0),
            egui::pos2(close_rect.left() + 11.0, close_rect.bottom() - 10.0),
        ],
        egui::Stroke::new(1.2, icon_color),
    );
    (
        log_response.clicked(),
        level_response.clicked(),
        minimize_response.clicked(),
        close_response.clicked(),
    )
}

fn draw_log_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.15, color);
    let page = egui::Rect::from_center_size(center, egui::vec2(10.0, 12.0));
    painter.rect_stroke(page, 1.0, stroke, egui::StrokeKind::Inside);
    for offset in [-3.0, 0.0, 3.0] {
        painter.line_segment(
            [
                egui::pos2(center.x - 3.0, center.y + offset),
                egui::pos2(center.x + 3.0, center.y + offset),
            ],
            stroke,
        );
    }
}

fn level_color(level: window_settings::WindowLevel) -> egui::Color32 {
    match level {
        window_settings::WindowLevel::Normal => GRAY,
        window_settings::WindowLevel::AlwaysOnTop => GREEN,
        window_settings::WindowLevel::AlwaysOnBottom => PASTEL_YELLOW,
    }
}

fn window_level_tooltip(level: window_settings::WindowLevel, language: Language) -> &'static str {
    match (level, language) {
        (window_settings::WindowLevel::Normal, Language::German) => {
            "Fensterebene: Normal\nKlicken: Immer im Vordergrund"
        }
        (window_settings::WindowLevel::AlwaysOnTop, Language::German) => {
            "Fensterebene: Immer im Vordergrund\nKlicken: Immer im Hintergrund"
        }
        (window_settings::WindowLevel::AlwaysOnBottom, Language::German) => {
            "Fensterebene: Immer im Hintergrund\nKlicken: Normal"
        }
        (window_settings::WindowLevel::Normal, Language::English) => {
            "Window level: Normal\nClick: Always on top"
        }
        (window_settings::WindowLevel::AlwaysOnTop, Language::English) => {
            "Window level: Always on top\nClick: Always in background"
        }
        (window_settings::WindowLevel::AlwaysOnBottom, Language::English) => {
            "Window level: Always in background\nClick: Normal"
        }
    }
}

fn window_level_message(level: window_settings::WindowLevel, language: Language) -> &'static str {
    match (level, language) {
        (window_settings::WindowLevel::Normal, Language::German) => "Fenster: normale Ebene",
        (window_settings::WindowLevel::AlwaysOnTop, Language::German) => {
            "Fenster bleibt im Vordergrund"
        }
        (window_settings::WindowLevel::AlwaysOnBottom, Language::German) => {
            "Fenster bleibt im Hintergrund"
        }
        (window_settings::WindowLevel::Normal, Language::English) => "Window: normal level",
        (window_settings::WindowLevel::AlwaysOnTop, Language::English) => "Window stays on top",
        (window_settings::WindowLevel::AlwaysOnBottom, Language::English) => {
            "Window stays in background"
        }
    }
}

fn draw_window_level_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    level: window_settings::WindowLevel,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.15, color);
    let offset = match level {
        window_settings::WindowLevel::Normal => 0.0,
        window_settings::WindowLevel::AlwaysOnTop => -1.5,
        window_settings::WindowLevel::AlwaysOnBottom => 1.5,
    };
    let back = egui::Rect::from_center_size(
        egui::pos2(center.x + 2.0, center.y + offset),
        egui::vec2(9.0, 7.0),
    );
    let front = egui::Rect::from_center_size(
        egui::pos2(center.x - 2.0, center.y - offset),
        egui::vec2(9.0, 7.0),
    );
    painter.rect_stroke(back, 1.0, stroke, egui::StrokeKind::Inside);
    painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Inside);
}

fn countdown_seconds(status: &WatchStatus) -> Option<u64> {
    match status {
        WatchStatus::ShutdownWarning {
            seconds_remaining, ..
        } => Some(*seconds_remaining),
        _ => None,
    }
}

fn format_countdown(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn night_mode_active(status: &WatchStatus) -> bool {
    matches!(
        status,
        WatchStatus::Watching { .. } | WatchStatus::ShutdownWarning { .. }
    )
}

fn completion_action_for_display(
    status: &WatchStatus,
    pending_action: Option<NightAction>,
) -> CompletionAction {
    match pending_action {
        Some(NightAction::SetCompletionAction(action)) => action,
        _ => backend::completion_action(status),
    }
}

fn action_for(status: &WatchStatus) -> NightAction {
    match status {
        WatchStatus::Off { .. } | WatchStatus::Finished { .. } => NightAction::Start,
        WatchStatus::Watching { .. } | WatchStatus::ShutdownWarning { .. } => NightAction::Stop,
    }
}

struct MoonView {
    color: egui::Color32,
    tooltip: String,
}

fn moon_view(
    status: &WatchStatus,
    pending_action: Option<NightAction>,
    error: Option<&str>,
    language: Language,
) -> MoonView {
    if matches!(pending_action, Some(NightAction::Start)) {
        return MoonView {
            color: GREEN,
            tooltip: language
                .text(
                    "Nachtmodus wird aktiviert …\nBitte kurz warten.",
                    "Night mode is being enabled …\nPlease wait.",
                )
                .into(),
        };
    }
    if matches!(pending_action, Some(NightAction::Stop)) {
        return MoonView {
            color: GRAY,
            tooltip: language
                .text(
                    "Nachtmodus wird deaktiviert …\nBitte kurz warten.",
                    "Night mode is being disabled …\nPlease wait.",
                )
                .into(),
        };
    }
    let (color, state, action) = match status {
        WatchStatus::Off { .. } => (
            PASTEL_YELLOW,
            language.text("Kein Nachtlauf aktiv", "No night run active"),
            language.text(
                "Klicken, um den Nachtmodus zu starten.",
                "Click to start night mode.",
            ),
        ),
        WatchStatus::Watching {
            observe_only,
            quiet,
            demo,
            ..
        } if *demo => (
            ACCENT,
            language.text("Demo läuft", "Demo running"),
            language.text(
                "Klicken, um die Demo zu stoppen.",
                "Click to stop the demo.",
            ),
        ),
        WatchStatus::Watching {
            observe_only,
            quiet,
            ..
        } => (
            if *observe_only { ACCENT } else { GREEN },
            if *observe_only {
                language.text(
                    "Beobachtung aktiv - kein Shutdown",
                    "Observation active - no shutdown",
                )
            } else if *quiet {
                language.text(
                    "Nachtmodus aktiv - Ruhezeit läuft",
                    "Night mode active - quiet period running",
                )
            } else {
                language.text("Nachtmodus aktiv", "Night mode active")
            },
            language.text(
                "Klicken, um den Nachtlauf zu stoppen.",
                "Click to stop the night run.",
            ),
        ),
        WatchStatus::ShutdownWarning {
            demo,
            observe_only,
            network_triggered,
            ..
        } => (
            if *observe_only { ACCENT } else { PASTEL_ORANGE },
            if *observe_only {
                language.text(
                    "Beobachtung abgeschlossen - keine Windows-Aktion",
                    "Observation complete - no Windows action",
                )
            } else if *network_triggered {
                language.text(
                    "Internet seit fünf Minuten nicht erreichbar - Warnung aktiv",
                    "Internet unavailable for five minutes - warning active",
                )
            } else if *demo {
                language.text("Demo-Warnung aktiv", "Demo warning active")
            } else {
                language.text("Shutdown-Warnung aktiv", "Shutdown warning active")
            },
            if *demo {
                language.text(
                    "Klicken, um die Demo zu stoppen.",
                    "Click to stop the demo.",
                )
            } else {
                language.text(
                    "Klicken, um Countdown und Nachtlauf abzubrechen.",
                    "Click to cancel the countdown and night run.",
                )
            },
        ),
        WatchStatus::Finished { outcome, .. } => (
            if outcome.contains("confirmed") {
                PASTEL_RED
            } else {
                PASTEL_YELLOW
            },
            if outcome.contains("confirmed") {
                language.text("Energieaktion wird ausgeführt", "Power action is executing")
            } else {
                language.text("Kein Nachtlauf aktiv", "No night run active")
            },
            language.text(
                "Klicken, um den Nachtmodus zu starten.",
                "Click to start night mode.",
            ),
        ),
    };
    let tooltip = if let Some(error) = error {
        format!("{state}\nAktion fehlgeschlagen: {error}\n{action}")
    } else {
        format!("{state}\n{action}")
    };
    MoonView { color, tooltip }
}

fn agents_for(status: &WatchStatus) -> &AgentSummary {
    match status {
        WatchStatus::Off { agents, .. }
        | WatchStatus::Watching { agents, .. }
        | WatchStatus::ShutdownWarning { agents, .. }
        | WatchStatus::Finished { agents, .. } => agents,
    }
}

fn completion_switch(ui: &mut egui::Ui, action: CompletionAction, enabled: bool) -> egui::Response {
    let size = egui::vec2(76.0, 26.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter();
    let border = egui::Color32::from_rgba_unmultiplied(148, 163, 184, 115);
    let track = egui::Color32::from_rgba_unmultiplied(15, 23, 42, 190);
    let selected = match action {
        CompletionAction::Sleep => GREEN,
        CompletionAction::Shutdown => RED,
    };
    let selected = if enabled {
        selected
    } else {
        selected.gamma_multiply(0.5)
    };
    painter.rect_filled(rect, 13.0, track);
    painter.rect_stroke(
        rect,
        13.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    glass_sheen(painter, rect);

    let knob_radius = 11.0;
    let left_center = egui::pos2(rect.left() + 14.0, rect.center().y);
    let right_center = egui::pos2(rect.right() - 14.0, rect.center().y);
    let knob_center = match action {
        CompletionAction::Sleep => left_center,
        CompletionAction::Shutdown => right_center,
    };
    painter.circle_filled(knob_center, knob_radius, selected);

    let inactive = GRAY.gamma_multiply(if enabled { 0.9 } else { 0.55 });
    let plug_color = if action == CompletionAction::Sleep {
        TEXT
    } else {
        inactive
    };
    let power_color = if action == CompletionAction::Shutdown {
        TEXT
    } else {
        inactive
    };
    draw_plug_icon(painter, left_center, plug_color);
    draw_power_icon(painter, right_center, power_color);
    response
}

fn draw_plug_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let body =
        egui::Rect::from_center_size(egui::pos2(center.x, center.y + 1.5), egui::vec2(7.0, 7.0));
    painter.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Inside);
    painter.line_segment(
        [
            egui::pos2(center.x - 2.2, center.y - 5.5),
            egui::pos2(center.x - 2.2, center.y - 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x + 2.2, center.y - 5.5),
            egui::pos2(center.x + 2.2, center.y - 2.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x, center.y + 5.0),
            egui::pos2(center.x, center.y + 7.0),
        ],
        stroke,
    );
}

fn draw_power_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    painter.circle_stroke(egui::pos2(center.x, center.y + 1.0), 5.0, stroke);
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - 6.5),
            egui::pos2(center.x, center.y + 0.5),
        ],
        egui::Stroke::new(2.0, color),
    );
}

fn moon_icon(
    ui: &mut egui::Ui,
    color: egui::Color32,
    diameter: f32,
    gradient_rect: egui::Rect,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click());
    let center = rect.center();
    let radius = diameter / 2.0;
    let painter = ui.painter();
    let halo_outer = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 18);
    let halo_inner = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32);
    painter.circle_filled(center, radius * 1.28, halo_outer);
    painter.circle_filled(center, radius * 1.14, halo_inner);
    painter.circle_filled(center, radius, color);
    let cutout_center = egui::pos2(center.x + radius * 0.45, center.y - radius * 0.28);
    painter.circle_filled(
        cutout_center,
        radius * 0.86,
        gradient_color_at(gradient_rect, cutout_center),
    );
    response
}

fn gradient_color_at(rect: egui::Rect, point: egui::Pos2) -> egui::Color32 {
    let position = ((point.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
    let blend = |top: u8, bottom: u8| -> u8 {
        (f32::from(top) + (f32::from(bottom) - f32::from(top)) * position).round() as u8
    };
    egui::Color32::from_rgb(
        blend(BG_TOP.r(), BG_BOTTOM.r()),
        blend(BG_TOP.g(), BG_BOTTOM.g()),
        blend(BG_TOP.b(), BG_BOTTOM.b()),
    )
}

fn metric(ui: &mut egui::Ui, label: &str, value: usize, color: egui::Color32) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(value.to_string())
                .size(26.0)
                .strong()
                .color(color),
        );
        ui.label(egui::RichText::new(label).small().color(GRAY));
    });
}

fn system_metrics_row(ui: &mut egui::Ui, metrics: SystemMetrics, language: Language) {
    let available = ui.available_width();
    let spacing = 7.0;
    let item_width = ((available - spacing * 4.0) / 5.0).max(52.0);
    let old_spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = spacing;
    ui.horizontal(|ui| {
        system_metric_badge(
            ui,
            item_width,
            MetricIcon::Cpu,
            "CPU",
            metrics.cpu_percent.map(|value| format!("{value:>2}%")),
            metric_color(metrics.cpu_percent),
            language,
        );
        system_metric_badge(
            ui,
            item_width,
            MetricIcon::Ram,
            "RAM",
            metrics.ram_percent.map(|value| format!("{value:>2}%")),
            metric_color(metrics.ram_percent),
            language,
        );
        system_metric_badge(
            ui,
            item_width,
            MetricIcon::Gpu,
            "GPU",
            metrics.gpu_percent.map(|value| format!("{value:>2}%")),
            metric_color(metrics.gpu_percent),
            language,
        );
        system_metric_badge(
            ui,
            item_width,
            MetricIcon::Vram,
            "VRAM",
            metrics.vram_percent.map(|value| format!("{value:>2}%")),
            metric_color(metrics.vram_percent),
            language,
        );
        system_metric_badge(
            ui,
            item_width,
            MetricIcon::Power,
            "",
            metrics.gpu_watts.map(|value| format!("{value:>3}W")),
            metric_color(metrics.gpu_power_percent),
            language,
        );
    });
    ui.spacing_mut().item_spacing.x = old_spacing;
}

fn metric_color(usage: Option<u8>) -> egui::Color32 {
    match usage {
        Some(value) if value >= 75 => PASTEL_RED,
        Some(value) if value >= 40 => PASTEL_YELLOW,
        Some(_) => PASTEL_GREEN,
        None => GRAY,
    }
}

#[derive(Clone, Copy)]
enum MetricIcon {
    Cpu,
    Gpu,
    Vram,
    Ram,
    Power,
}

fn system_metric_badge(
    ui: &mut egui::Ui,
    width: f32,
    icon: MetricIcon,
    label: &str,
    value: Option<String>,
    color: egui::Color32,
    language: Language,
) {
    let tooltip = match value.as_deref() {
        Some(value) if matches!(icon, MetricIcon::Vram) => {
            format!(
                "{}{}",
                language.text("VRAM-Auslastung: ", "VRAM utilization: "),
                value
            )
        }
        Some(value) if label.is_empty() => {
            format!(
                "{}{}",
                language.text("Grafikkartenverbrauch: ", "GPU power draw: "),
                value
            )
        }
        Some(value) => format!(
            "{label} {}: {value}",
            language.text("Auslastung", "utilization")
        ),
        None if matches!(icon, MetricIcon::Vram) => language
            .text(
                "VRAM-Wert ist momentan nicht verfügbar.",
                "VRAM value is currently unavailable.",
            )
            .into(),
        None if label.is_empty() => language
            .text(
                "Grafikkartenverbrauch ist für diese Hardware nicht verfügbar.",
                "GPU power draw is unavailable on this hardware.",
            )
            .into(),
        None => format!(
            "{label} {}",
            language.text("ist momentan nicht verfügbar.", "is currently unavailable.")
        ),
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
    draw_metric_icon(
        ui.painter(),
        icon,
        egui::pos2(rect.left() + 7.0, rect.center().y),
        color,
    );
    let value = value.unwrap_or_else(|| "—".into());
    let text = if label.is_empty() {
        value.clone()
    } else {
        format!("{label} {value}")
    };
    ui.painter().text(
        egui::pos2(rect.left() + 17.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.0),
        if value == "—" { GRAY } else { color },
    );
    response.on_hover_text(tooltip);
}

fn draw_metric_icon(
    painter: &egui::Painter,
    icon: MetricIcon,
    center: egui::Pos2,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.1, color);
    match icon {
        MetricIcon::Cpu | MetricIcon::Vram | MetricIcon::Ram => {
            let size = if matches!(icon, MetricIcon::Cpu) {
                7.0
            } else {
                8.0
            };
            let body = egui::Rect::from_center_size(center, egui::vec2(size, size));
            painter.rect_stroke(body, 1.2, stroke, egui::StrokeKind::Inside);
            for offset in [-3.0, 0.0, 3.0] {
                painter.line_segment(
                    [
                        egui::pos2(center.x + offset, center.y - 6.0),
                        egui::pos2(center.x + offset, center.y - 4.0),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        egui::pos2(center.x + offset, center.y + 4.0),
                        egui::pos2(center.x + offset, center.y + 6.0),
                    ],
                    stroke,
                );
            }
            if matches!(icon, MetricIcon::Vram) {
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    "GPU",
                    egui::FontId::proportional(4.5),
                    color,
                );
            }
        }
        MetricIcon::Gpu => {
            let body = egui::Rect::from_center_size(center, egui::vec2(10.0, 7.0));
            painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(center.x - 2.0, center.y - 2.0),
                    egui::pos2(center.x + 2.0, center.y + 2.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x + 2.0, center.y - 2.0),
                    egui::pos2(center.x - 2.0, center.y + 2.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x + 5.0, center.y - 2.0),
                    egui::pos2(center.x + 7.0, center.y - 2.0),
                ],
                stroke,
            );
        }
        MetricIcon::Power => {
            let points = vec![
                egui::pos2(center.x + 2.0, center.y - 7.0),
                egui::pos2(center.x - 2.0, center.y - 1.0),
                egui::pos2(center.x + 1.0, center.y - 1.0),
                egui::pos2(center.x - 2.0, center.y + 7.0),
                egui::pos2(center.x + 4.0, center.y - 2.0),
                egui::pos2(center.x + 1.0, center.y - 2.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                color,
                egui::Stroke::NONE,
            ));
        }
    }
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(10.0);
}

fn glassy_frame(ui: &mut egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(120, 140, 180, 50),
        ))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::same(12))
}

fn glass_sheen(painter: &egui::Painter, rect: egui::Rect) {
    let inset = 12.0_f32.min(rect.width() / 4.0);
    let band_bottom = (rect.top() + rect.height() * 0.18).min(rect.bottom() - 1.0);
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 1.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 1.0, band_bottom),
    );
    painter.rect_filled(
        band,
        egui::CornerRadius {
            nw: 9,
            ne: 9,
            sw: 0,
            se: 0,
        },
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + inset, rect.top() + 1.5),
            egui::pos2(rect.right() - inset, rect.top() + 1.5),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
        ),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left() + inset + 8.0, rect.top() + 3.0),
            egui::pos2(rect.right() - inset - 8.0, rect.top() + 3.0),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
        ),
    );
}

fn paint_gradient(
    painter: &egui::Painter,
    rect: egui::Rect,
    top: egui::Color32,
    bottom: egui::Color32,
) {
    let mesh = egui::epaint::Mesh {
        vertices: vec![
            egui::epaint::Vertex {
                pos: rect.left_top(),
                uv: egui::Pos2::ZERO,
                color: top,
            },
            egui::epaint::Vertex {
                pos: rect.right_top(),
                uv: egui::Pos2::ZERO,
                color: top,
            },
            egui::epaint::Vertex {
                pos: rect.right_bottom(),
                uv: egui::Pos2::ZERO,
                color: bottom,
            },
            egui::epaint::Vertex {
                pos: rect.left_bottom(),
                uv: egui::Pos2::ZERO,
                color: bottom,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    painter.add(egui::epaint::Shape::Mesh(std::sync::Arc::new(mesh)));
}

fn configure_visuals(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.extreme_bg_color = BG_BOTTOM;
    visuals.faint_bg_color = egui::Color32::from_rgb(28, 36, 56);
    visuals.code_bg_color = egui::Color32::from_rgb(20, 26, 42);
    visuals.widgets.noninteractive.bg_fill = BG_BOTTOM;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(59, 130, 246, 110);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT_STRONG;
    context.set_visuals(visuals);
}
