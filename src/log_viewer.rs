use crate::{language::Language, window_settings};
use anyhow::{Context, Result};
use eframe::egui;
use std::fs;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::Duration;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetWindowLongW, LWA_ALPHA, SetLayeredWindowAttributes,
    SetWindowLongW, WS_EX_LAYERED,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);
const GREEN: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);
const GRAY: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 232, 240);
const BG_TOP: egui::Color32 = egui::Color32::from_rgb(26, 34, 54);
const BG_BOTTOM: egui::Color32 = egui::Color32::from_rgb(14, 19, 33);

pub fn open() -> Result<()> {
    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    Command::new(executable)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--completion-log")
        .spawn()
        .context("Abschlussprotokoll konnte nicht geöffnet werden")?;
    Ok(())
}

pub fn run() -> Result<()> {
    let language = Language::current();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 430.0])
            .with_min_inner_size([500.0, 280.0])
            .with_decorations(false)
            .with_window_level(window_level(window_settings::WindowLevel::current()))
            .with_title(log_title(language)),
        ..Default::default()
    };
    eframe::run_native(
        log_title(language),
        options,
        Box::new(|creation_context| {
            configure_visuals(&creation_context.egui_ctx);
            Ok(Box::new(LogApp::new()))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Abschlussprotokoll konnte nicht ausgeführt werden: {error}"))
}

fn log_title(language: Language) -> &'static str {
    language.text(
        "Herdr-Nachtwächter - Abschlussprotokoll",
        "Herdr Night Watch - Completion Log",
    )
}

fn log_path() -> Result<std::path::PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("Installationsordner konnte nicht bestimmt werden")?
        .join("logs")
        .join("completion-history.csv"))
}

#[derive(Clone)]
struct LogEntry {
    timestamp: String,
    action: String,
    trigger: String,
    run_id: String,
}

struct LogApp {
    language: Language,
    entries: Vec<LogEntry>,
    error: Option<String>,
    opacity: Option<u8>,
}

impl LogApp {
    fn new() -> Self {
        match load_entries() {
            Ok(entries) => Self {
                language: Language::current(),
                entries,
                error: None,
                opacity: None,
            },
            Err(error) => Self {
                language: Language::current(),
                entries: Vec::new(),
                error: Some(error.to_string()),
                opacity: None,
            },
        }
    }
}

impl eframe::App for LogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let current_language = Language::current();
        if current_language != self.language {
            self.language = current_language;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(
                log_title(current_language).into(),
            ));
        }
        let current_opacity = window_settings::opacity();
        if self.opacity != Some(current_opacity) {
            apply_window_opacity(current_opacity, log_title(self.language));
            self.opacity = Some(current_opacity);
        }

        let window_rect = ui.max_rect();
        paint_gradient(ui.painter(), window_rect);
        let header_rect = egui::Rect::from_min_max(
            egui::pos2(window_rect.left() + 1.0, window_rect.top() + 1.0),
            egui::pos2(window_rect.right() - 1.0, window_rect.top() + 48.0),
        );
        glass_sheen(ui.painter(), header_rect);
        let (minimize, close) = window_controls(ui);
        if minimize {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if close {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(18, 16))
            .show(ui, |ui| {
                ui.heading(
                    egui::RichText::new(self.language.text("Abschlussprotokoll", "Completion log"))
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(self.language.text(
                        "Die letzten 30 angeforderten Energieaktionen",
                        "The last 30 requested power actions",
                    ))
                    .color(GRAY),
                );
                ui.add_space(12.0);
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(120, 140, 180, 50),
                    ))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        if let Some(error) = &self.error {
                            ui.colored_label(RED, error);
                        } else if self.entries.is_empty() {
                            ui.label(
                                self.language
                                    .text("Noch keine Einträge.", "No entries yet."),
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (index, entry) in self.entries.iter().enumerate() {
                                        if index > 0 {
                                            ui.separator();
                                        }
                                        log_entry_row(ui, entry, self.language);
                                    }
                                });
                        }
                    });
            });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn load_entries() -> Result<Vec<LogEntry>> {
    let path = log_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        fs::read_to_string(path).context("Abschlussprotokoll konnte nicht gelesen werden")?;
    let mut entries: Vec<_> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split(';').collect();
            (fields.len() >= 4).then(|| LogEntry {
                timestamp: fields[0].to_string(),
                action: fields[1].to_string(),
                trigger: fields[2].to_string(),
                run_id: fields[3].to_string(),
            })
        })
        .collect();
    entries.reverse();
    entries.truncate(30);
    Ok(entries)
}

fn log_entry_row(ui: &mut egui::Ui, entry: &LogEntry, language: Language) {
    let color = if entry.action.contains("Energiespar") {
        GREEN
    } else if entry.action.contains("Herunter") {
        RED
    } else {
        ACCENT
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&entry.timestamp).small().color(GRAY));
        ui.colored_label(color, localized_action(&entry.action, language));
        ui.colored_label(GRAY, localized_trigger(&entry.trigger, language));
    });
    ui.label(
        egui::RichText::new(format!("Lauf-ID: {}", entry.run_id))
            .small()
            .color(GRAY),
    );
}

fn localized_action(action: &str, language: Language) -> &str {
    match (action, language) {
        ("Energiesparmodus angefordert", Language::English) => "Sleep requested",
        ("Herunterfahren angefordert", Language::English) => "Shutdown requested",
        _ => action,
    }
}

fn localized_trigger(trigger: &str, language: Language) -> &str {
    match (trigger, language) {
        ("Herdr-Agenten fertig", Language::English) => "Herdr agents finished",
        ("Internet seit 5 Minuten nicht erreichbar", Language::English) => {
            "Internet unavailable for 5 minutes"
        }
        ("Sofortbestätigung", Language::English) => "Immediate confirmation",
        _ => trigger,
    }
}

fn window_controls(ui: &mut egui::Ui) -> (bool, bool) {
    let rect = ui.max_rect();
    let hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 68.0, rect.top() + 3.0),
        egui::pos2(rect.right() - 6.0, rect.top() + 29.0),
    );
    let drag = rect;
    if ui
        .interact(
            drag,
            ui.make_persistent_id("log_window_drag"),
            egui::Sense::drag(),
        )
        .drag_started()
    {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let minimize_rect = egui::Rect::from_min_max(
        egui::pos2(hood.left() + 2.0, hood.top()),
        egui::pos2(hood.left() + 32.0, hood.bottom()),
    );
    let close_rect = egui::Rect::from_min_max(
        egui::pos2(hood.right() - 32.0, hood.top()),
        egui::pos2(hood.right() - 2.0, hood.bottom()),
    );
    ui.painter().rect_filled(
        hood,
        egui::CornerRadius::same(10),
        egui::Color32::from_rgba_unmultiplied(28, 29, 38, 220),
    );
    let minimize = ui.interact(
        minimize_rect,
        ui.make_persistent_id("log_window_minimize"),
        egui::Sense::click(),
    );
    let close = ui.interact(
        close_rect,
        ui.make_persistent_id("log_window_close"),
        egui::Sense::click(),
    );
    let color = if close.hovered() {
        egui::Color32::WHITE
    } else {
        GRAY
    };
    ui.painter().line_segment(
        [
            egui::pos2(minimize_rect.left() + 11.0, minimize_rect.center().y + 4.0),
            egui::pos2(minimize_rect.right() - 11.0, minimize_rect.center().y + 4.0),
        ],
        egui::Stroke::new(1.2, color),
    );
    ui.painter().line_segment(
        [
            egui::pos2(close_rect.left() + 10.0, close_rect.top() + 9.0),
            egui::pos2(close_rect.right() - 10.0, close_rect.bottom() - 9.0),
        ],
        egui::Stroke::new(1.2, color),
    );
    ui.painter().line_segment(
        [
            egui::pos2(close_rect.right() - 10.0, close_rect.top() + 9.0),
            egui::pos2(close_rect.left() + 10.0, close_rect.bottom() - 9.0),
        ],
        egui::Stroke::new(1.2, color),
    );
    (minimize.clicked(), close.clicked())
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

fn window_level(level: window_settings::WindowLevel) -> egui::WindowLevel {
    match level {
        window_settings::WindowLevel::Normal => egui::WindowLevel::Normal,
        window_settings::WindowLevel::AlwaysOnTop => egui::WindowLevel::AlwaysOnTop,
        window_settings::WindowLevel::AlwaysOnBottom => egui::WindowLevel::AlwaysOnBottom,
    }
}

fn paint_gradient(painter: &egui::Painter, rect: egui::Rect) {
    let mesh = egui::epaint::Mesh {
        vertices: vec![
            egui::epaint::Vertex {
                pos: rect.left_top(),
                uv: egui::Pos2::ZERO,
                color: BG_TOP,
            },
            egui::epaint::Vertex {
                pos: rect.right_top(),
                uv: egui::Pos2::ZERO,
                color: BG_TOP,
            },
            egui::epaint::Vertex {
                pos: rect.right_bottom(),
                uv: egui::Pos2::ZERO,
                color: BG_BOTTOM,
            },
            egui::epaint::Vertex {
                pos: rect.left_bottom(),
                uv: egui::Pos2::ZERO,
                color: BG_BOTTOM,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    painter.add(egui::epaint::Shape::Mesh(std::sync::Arc::new(mesh)));
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

fn configure_visuals(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.extreme_bg_color = BG_BOTTOM;
    visuals.faint_bg_color = egui::Color32::from_rgb(28, 36, 56);
    visuals.widgets.noninteractive.bg_fill = BG_BOTTOM;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    context.set_visuals(visuals);
}
