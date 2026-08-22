use crate::{language::Language, window_chrome, window_settings};
use anyhow::{Context, Result};
use eframe::egui;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);
const GREEN: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);
const GRAY: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 232, 240);
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
            .with_inner_size([700.0, 450.0])
            .with_min_inner_size([580.0, 300.0])
            .with_resizable(true)
            .with_decorations(false)
            .with_window_level(window_chrome::window_level(
                window_settings::WindowLevel::current(),
            ))
            .with_title(log_title(language)),
        renderer: eframe::Renderer::Glow,
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

fn log_paths() -> Result<[PathBuf; 2]> {
    let directory = std::env::current_exe()?
        .parent()
        .context("Installationsordner konnte nicht bestimmt werden")?
        .join("logs");
    Ok([
        directory.join("completion-history.csv"),
        directory.join("tray-history.csv"),
    ])
}

#[derive(Clone)]
struct LogEntry {
    timestamp: String,
    action: String,
    trigger: String,
    _run_id: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogSection {
    Energy,
    Tray,
}

struct LogApp {
    language: Language,
    energy_entries: Vec<LogEntry>,
    tray_entries: Vec<LogEntry>,
    section: LogSection,
    error: Option<String>,
    opacity: Option<u8>,
    resize_drag: Option<(egui::Pos2, egui::Vec2)>,
}

impl LogApp {
    fn new() -> Self {
        let language = Language::current();
        match load_entries() {
            Ok((energy_entries, tray_entries)) => Self {
                language,
                energy_entries,
                tray_entries,
                section: LogSection::Energy,
                error: None,
                opacity: None,
                resize_drag: None,
            },
            Err(_) => Self {
                language,
                energy_entries: Vec::new(),
                tray_entries: Vec::new(),
                section: LogSection::Energy,
                error: Some(
                    language
                        .text(
                            "Abschlussprotokoll konnte nicht gelesen werden.",
                            "The completion log could not be read.",
                        )
                        .to_owned(),
                ),
                opacity: None,
                resize_drag: None,
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
            window_chrome::apply_window_opacity(current_opacity, log_title(self.language));
            self.opacity = Some(current_opacity);
        }

        let window_rect = ui.max_rect();
        window_chrome::default_gradient(ui.painter(), window_rect);
        handle_window_resize(self, ui);
        let header_rect = egui::Rect::from_min_max(
            egui::pos2(window_rect.left() + 1.0, window_rect.top() + 1.0),
            egui::pos2(window_rect.right() - 1.0, window_rect.top() + 48.0),
        );
        window_chrome::glass_sheen(ui.painter(), header_rect);
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
                        "Energieaktionen und Tray-Diagnose getrennt",
                        "Power actions and tray diagnostics, separated",
                    ))
                    .color(GRAY),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let energy_count = self.energy_entries.len();
                    let tray_count = self.tray_entries.len();
                    let energy_label = format!(
                        "{} ({energy_count})",
                        self.language.text("Energieaktionen", "Power actions")
                    );
                    let tray_label = format!(
                        "{} ({tray_count})",
                        self.language
                            .text("Tray und Diagnose", "Tray and diagnostics")
                    );
                    if ui
                        .selectable_label(self.section == LogSection::Energy, energy_label)
                        .clicked()
                    {
                        self.section = LogSection::Energy;
                    }
                    if ui
                        .selectable_label(self.section == LogSection::Tray, tray_label)
                        .clicked()
                    {
                        self.section = LogSection::Tray;
                    }
                });
                ui.add_space(8.0);
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
                        } else {
                            let entries = match self.section {
                                LogSection::Energy => &self.energy_entries,
                                LogSection::Tray => &self.tray_entries,
                            };
                            let empty_message = match self.section {
                                LogSection::Energy => self
                                    .language
                                    .text("Noch keine Energieaktionen.", "No power actions yet."),
                                LogSection::Tray => self.language.text(
                                    "Noch keine Tray- oder Diagnoseereignisse.",
                                    "No tray or diagnostic events yet.",
                                ),
                            };
                            if entries.is_empty() {
                                ui.label(empty_message);
                                return;
                            }
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    log_table(ui, entries, self.language);
                                });
                        }
                    });
            });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn load_entries() -> Result<(Vec<LogEntry>, Vec<LogEntry>)> {
    let paths = log_paths()?;
    let completion_entries = load_entries_from(&paths[0])?;
    // Older builds occasionally wrote a tray diagnostic into the completion
    // file. Classify by action as well as by file so the UI stays correct for
    // existing installations without rewriting the user's history.
    let mut energy_entries = completion_entries
        .iter()
        .filter(|entry| !is_tray_entry(entry))
        .cloned()
        .collect::<Vec<_>>();
    let mut tray_entries = load_entries_from(&paths[1])?;
    tray_entries.extend(completion_entries.into_iter().filter(is_tray_entry));
    for entries in [&mut energy_entries, &mut tray_entries] {
        entries.sort_unstable_by(|left, right| right.timestamp.cmp(&left.timestamp));
        entries.truncate(30);
    }
    Ok((energy_entries, tray_entries))
}

fn load_entries_from(path: &Path) -> Result<Vec<LogEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("Protokoll konnte nicht gelesen werden: {}", path.display()))?;
    let entries: Vec<_> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split(';').collect();
            (fields.len() >= 4).then(|| LogEntry {
                timestamp: fields[0].to_string(),
                action: fields[1].to_string(),
                trigger: fields[2].to_string(),
                _run_id: fields[3].to_string(),
            })
        })
        .collect();
    Ok(entries)
}

fn is_tray_entry(entry: &LogEntry) -> bool {
    entry.action.contains("Tray-App")
        || entry.action.contains("Tray app")
        || entry.trigger.contains("Vorherige Sitzung")
        || entry.trigger.contains("Previous session")
}

fn log_table(ui: &mut egui::Ui, entries: &[LogEntry], language: Language) {
    egui::Grid::new("completion_log_table")
        .num_columns(5)
        .spacing([14.0, 9.0])
        .striped(true)
        .show(ui, |ui| {
            for header in [
                language.text("Datum", "Date"),
                language.text("Uhrzeit", "Time"),
                language.text("Aktion", "Action"),
                language.text("Ergebnis", "Result"),
                language.text("Erklärung", "Explanation"),
            ] {
                ui.label(
                    egui::RichText::new(header)
                        .font(egui::FontId::proportional(14.0))
                        .strong()
                        .color(GRAY),
                );
            }
            ui.end_row();
            for entry in entries {
                let (date, time) = display_date_time(&entry.timestamp, language);
                let (_, result_color) = localized_result(&entry.action, language);
                let trigger = localized_trigger(&entry.trigger, language);
                let explanation = if trigger.is_empty() {
                    localized_action(&entry.action, language).to_owned()
                } else {
                    trigger.to_owned()
                };
                ui.add_sized(
                    [102.0, 22.0],
                    egui::Label::new(
                        egui::RichText::new(date)
                            .font(egui::FontId::proportional(13.0))
                            .color(TEXT),
                    ),
                );
                ui.add_sized(
                    [74.0, 22.0],
                    egui::Label::new(
                        egui::RichText::new(time)
                            .font(egui::FontId::proportional(13.0))
                            .color(GRAY),
                    ),
                );
                egui::Frame::new()
                    .fill(action_background(&entry.action))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.add_sized(
                            [108.0, 18.0],
                            egui::Label::new(
                                egui::RichText::new(localized_action_kind(&entry.action, language))
                                    .font(egui::FontId::proportional(13.0))
                                    .color(TEXT),
                            ),
                        );
                    });
                draw_result_icon(ui, &entry.action, result_color);
                ui.label(
                    egui::RichText::new(explanation)
                        .font(egui::FontId::proportional(13.0))
                        .color(TEXT),
                );
                ui.end_row();
            }
        });
}

fn handle_window_resize(app: &mut LogApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let grip = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 28.0, rect.bottom() - 28.0),
        rect.right_bottom(),
    );
    let response = ui.interact(
        grip,
        ui.make_persistent_id("log_window_resize"),
        egui::Sense::drag(),
    );
    if response.drag_started() {
        let pointer = ui
            .ctx()
            .input(|input| input.pointer.interact_pos())
            .unwrap_or(grip.right_bottom());
        app.resize_drag = Some((pointer, rect.size()));
    }
    if let Some((start, initial_size)) = app.resize_drag {
        if let Some(pointer) = ui.ctx().input(|input| input.pointer.interact_pos()) {
            let delta = pointer - start;
            let size = egui::vec2(
                (initial_size.x + delta.x).max(580.0),
                (initial_size.y + delta.y).max(300.0),
            );
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
        if response.drag_stopped() {
            app.resize_drag = None;
        }
    }
    let color = if response.hovered() || response.dragged() {
        egui::Color32::from_rgba_unmultiplied(226, 232, 240, 170)
    } else {
        egui::Color32::from_rgba_unmultiplied(148, 163, 184, 80)
    };
    for offset in [0.0, 6.0, 12.0] {
        ui.painter().line_segment(
            [
                egui::pos2(grip.right() - 5.0 - offset, grip.bottom() - 1.0),
                egui::pos2(grip.right() - 1.0, grip.bottom() - 5.0 - offset),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
}

fn display_date_time(timestamp: &str, language: Language) -> (String, String) {
    let mut parts = timestamp.split_whitespace();
    let date = parts.next().unwrap_or_default();
    let time = parts.next().unwrap_or_default().to_owned();
    let date_parts: Vec<_> = date.split('-').collect();
    let formatted_date = if date_parts.len() == 3 {
        match language {
            Language::German => format!(
                "{:0>2}.{:0>2}.{}",
                date_parts[2], date_parts[1], date_parts[0]
            ),
            Language::English => format!(
                "{:0>2}/{:0>2}/{}",
                date_parts[1], date_parts[2], date_parts[0]
            ),
        }
    } else {
        date.to_owned()
    };
    (formatted_date, time)
}

fn localized_result(action: &str, _language: Language) -> (&'static str, egui::Color32) {
    let is_error = action.contains("unplanmäßig")
        || action.contains("unexpected")
        || action.contains("fehlgeschlagen")
        || action.contains("failed");
    if is_error {
        return ("✕", RED);
    }
    if action.contains("Energiespar")
        || action.contains("Herunter")
        || action.contains("requested")
        || action.contains("angefordert")
    {
        return ("✓", GREEN);
    }
    ("•", ACCENT)
}

fn draw_result_icon(ui: &mut egui::Ui, action: &str, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(112.0, 22.0), egui::Sense::hover());
    let center = rect.center();
    let half = 6.5;
    let stroke = egui::Stroke::new(2.0, color);
    if action.contains("unplanmäßig")
        || action.contains("unexpected")
        || action.contains("fehlgeschlagen")
        || action.contains("failed")
    {
        ui.painter().line_segment(
            [
                egui::pos2(center.x - half, center.y - half),
                egui::pos2(center.x + half, center.y + half),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + half, center.y - half),
                egui::pos2(center.x - half, center.y + half),
            ],
            stroke,
        );
    } else if action.contains("Energiespar")
        || action.contains("Herunter")
        || action.contains("requested")
        || action.contains("angefordert")
    {
        ui.painter().line_segment(
            [
                egui::pos2(center.x - half, center.y),
                egui::pos2(center.x - 2.0, center.y + half - 1.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x - 2.0, center.y + half - 1.0),
                egui::pos2(center.x + half, center.y - half),
            ],
            stroke,
        );
    } else {
        ui.painter().circle_filled(center, 3.0, color);
    }
}

fn action_background(action: &str) -> egui::Color32 {
    if action.contains("Energiespar") || action.contains("Sleep") {
        egui::Color32::from_rgba_unmultiplied(74, 222, 128, 42)
    } else if action.contains("Herunter")
        || action.contains("Shutdown")
        || action.contains("unplanmäßig")
        || action.contains("unexpected")
    {
        egui::Color32::from_rgba_unmultiplied(248, 113, 113, 42)
    } else {
        egui::Color32::from_rgba_unmultiplied(148, 163, 184, 18)
    }
}

fn localized_action_kind(action: &str, language: Language) -> &str {
    match (action, language) {
        ("Energiesparmodus angefordert", Language::English) => "Sleep mode",
        ("Herunterfahren angefordert", Language::English) => "Shutdown",
        ("Tray-App unplanmäßig beendet", Language::English) => "Tray app",
        ("Energiesparmodus angefordert", Language::German) => "Energiesparmodus",
        ("Herunterfahren angefordert", Language::German) => "Herunterfahren",
        ("Tray-App unplanmäßig beendet", Language::German) => "Tray-App",
        _ => action,
    }
}

fn localized_action(action: &str, language: Language) -> &str {
    match (action, language) {
        ("Energiesparmodus angefordert", Language::English) => "Sleep requested",
        ("Herunterfahren angefordert", Language::English) => "Shutdown requested",
        ("Tray-App unplanmäßig beendet", Language::English) => "Tray app ended unexpectedly",
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
        ("Vorherige Sitzung ohne sauberes Ende", Language::English) => {
            "Previous session had no clean exit"
        }
        _ => trigger,
    }
}

fn window_controls(ui: &mut egui::Ui) -> (bool, bool) {
    let rect = ui.max_rect();
    let resize_grip = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 28.0, rect.bottom() - 28.0),
        rect.right_bottom(),
    );
    let hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 68.0, rect.top() + 3.0),
        egui::pos2(rect.right() - 6.0, rect.top() + 29.0),
    );
    let drag = rect;
    let pointer_over_resize_grip = ui
        .ctx()
        .input(|input| input.pointer.interact_pos())
        .is_some_and(|pointer| resize_grip.contains(pointer));
    if !pointer_over_resize_grip
        && ui
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
    window_chrome::glass_sheen(ui.painter(), hood);
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

fn configure_visuals(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.extreme_bg_color = BG_BOTTOM;
    visuals.faint_bg_color = egui::Color32::from_rgb(28, 36, 56);
    visuals.widgets.noninteractive.bg_fill = BG_BOTTOM;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    context.set_visuals(visuals);
}
