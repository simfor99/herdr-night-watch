use crate::{
    language::Language,
    weather::{self, WeatherLocation},
    window_chrome, window_settings,
};
use anyhow::{Context, Result};
use eframe::egui;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 165, 250);
const RED: egui::Color32 = egui::Color32::from_rgb(242, 145, 150);
const GRAY: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 232, 240);
const BG_BOTTOM: egui::Color32 = egui::Color32::from_rgb(14, 19, 33);

pub fn open() -> Result<()> {
    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    Command::new(executable)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--weather-location")
        .spawn()
        .context("Wetterort konnte nicht geöffnet werden")?;
    Ok(())
}

pub fn run() -> Result<()> {
    let language = Language::current();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 390.0])
            .with_min_inner_size([460.0, 320.0])
            .with_decorations(false)
            .with_window_level(window_chrome::window_level(
                window_settings::WindowLevel::current(),
            ))
            .with_title(window_title(language)),
        ..Default::default()
    };
    eframe::run_native(
        window_title(language),
        options,
        Box::new(|creation_context| {
            configure_visuals(&creation_context.egui_ctx);
            Ok(Box::new(LocationApp::new()))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Wetterort konnte nicht ausgeführt werden: {error}"))
}

fn window_title(language: Language) -> &'static str {
    language.text(
        "Herdr-Nachtwächter - Wetterort",
        "Herdr Night Watch - Weather location",
    )
}

struct LocationApp {
    language: Language,
    selected: WeatherLocation,
    query: String,
    results: Vec<WeatherLocation>,
    search_tx: Sender<Result<Vec<WeatherLocation>, String>>,
    search_rx: Receiver<Result<Vec<WeatherLocation>, String>>,
    searching: bool,
    last_query: String,
    search_started: Option<Instant>,
    error: Option<String>,
    opacity: Option<u8>,
}

impl LocationApp {
    fn new() -> Self {
        let (search_tx, search_rx) = mpsc::channel();
        let selected = weather::current_location();
        Self {
            language: Language::current(),
            selected,
            query: String::new(),
            results: Vec::new(),
            search_tx,
            search_rx,
            searching: false,
            last_query: String::new(),
            search_started: None,
            error: None,
            opacity: None,
        }
    }

    fn start_search(&mut self) {
        let query = self.query.trim().to_owned();
        if query.len() < 2 || self.searching || query == self.last_query {
            if query.len() < 2 {
                self.search_started = None;
            }
            return;
        }
        self.last_query = query.clone();
        self.searching = true;
        self.search_started = None;
        self.error = None;
        let sender = self.search_tx.clone();
        let language = self.language;
        std::thread::spawn(move || {
            let result =
                weather::search_locations(&query, language).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
    }

    fn collect_search(&mut self) {
        while let Ok(result) = self.search_rx.try_recv() {
            self.searching = false;
            self.search_started = (self.query.trim() != self.last_query).then(Instant::now);
            match result {
                Ok(results) => self.results = results,
                Err(error) => {
                    self.results.clear();
                    self.error = Some(error);
                }
            }
        }
    }

    fn choose(&mut self, location: WeatherLocation, ctx: &egui::Context) {
        match weather::save_location(&location) {
            Ok(()) => {
                self.selected = location;
                self.error = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}

impl eframe::App for LocationApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let language = Language::current();
        if language != self.language {
            self.language = language;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(window_title(language).into()));
        }
        let opacity = window_settings::opacity();
        if self.opacity != Some(opacity) {
            window_chrome::apply_window_opacity(opacity, window_title(self.language));
            self.opacity = Some(opacity);
        }
        self.collect_search();
        if self
            .search_started
            .is_some_and(|started| started.elapsed() >= Duration::from_millis(250))
        {
            self.start_search();
        }

        let rect = ui.max_rect();
        window_chrome::default_gradient(ui.painter(), rect);
        let header = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 1.0, rect.top() + 1.0),
            egui::pos2(rect.right() - 1.0, rect.top() + 62.0),
        );
        window_chrome::glass_sheen(ui.painter(), header);
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
                    egui::RichText::new(self.language.text("Wetterort", "Weather location"))
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(self.language.text(
                        "Der Ort für die Temperaturanzeige im Mond",
                        "The location used for the moon's temperature",
                    ))
                    .color(GRAY),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(self.language.text("Aktueller Ort", "Current location"))
                        .color(ACCENT),
                );
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(location_label(&self.selected))
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.4}, {:.4}",
                            self.selected.latitude, self.selected.longitude
                        ))
                        .small()
                        .color(GRAY),
                    );
                });
                ui.add_space(12.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text(self.language.text(
                            "Stadt oder Postleitzahl suchen …",
                            "Search city or postal code …",
                        ))
                        .desired_width(ui.available_width() - 2.0),
                );
                if response.changed() {
                    self.search_started = Some(Instant::now());
                    self.last_query.clear();
                }
                if self.searching {
                    ui.label(
                        egui::RichText::new(self.language.text("Suche läuft …", "Searching …"))
                            .small()
                            .color(GRAY),
                    );
                } else if self.query.trim().len() < 2 {
                    ui.label(
                        egui::RichText::new(self.language.text(
                            "Mindestens zwei Zeichen eingeben.",
                            "Enter at least two characters.",
                        ))
                        .small()
                        .color(GRAY),
                    );
                }
                if let Some(error) = &self.error {
                    ui.colored_label(RED, error);
                }
                ui.add_space(8.0);
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(120, 140, 180, 50),
                    ))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        if self.results.is_empty() {
                            ui.label(
                                egui::RichText::new(self.language.text(
                                    "Suchergebnisse erscheinen hier.",
                                    "Search results appear here.",
                                ))
                                .color(GRAY),
                            );
                        } else {
                            let results = self.results.clone();
                            egui::ScrollArea::vertical()
                                .max_height(154.0)
                                .show(ui, |ui| {
                                    for location in results {
                                        let response = ui.add_sized(
                                            [ui.available_width(), 38.0],
                                            egui::Button::new(
                                                egui::RichText::new(format_location(&location))
                                                    .color(TEXT),
                                            )
                                            .fill(egui::Color32::TRANSPARENT),
                                        );
                                        if response.clicked() {
                                            self.choose(location, ui.ctx());
                                        }
                                    }
                                });
                        }
                    });
                ui.add_space(8.0);
                ui.label(egui::RichText::new(self.language.text(
                    "Die Temperatur ist rein informativ. Sie beeinflusst den Nachtwächter nicht.",
                    "The temperature is informational only. It never affects Night Watch.",
                )).small().color(GRAY));
                ui.label(
                    egui::RichText::new("Weather data by Open-Meteo · open-meteo.com")
                        .small()
                        .color(GRAY),
                );
            });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}

fn location_label(location: &WeatherLocation) -> String {
    if location.country.is_empty() {
        location.name.clone()
    } else {
        format!("{}, {}", location.name, location.country)
    }
}

fn format_location(location: &WeatherLocation) -> String {
    let region = if location.admin1.is_empty() {
        &location.country
    } else {
        &location.admin1
    };
    if region.is_empty() {
        location.name.clone()
    } else {
        format!("{} · {}", location.name, region)
    }
}

fn window_controls(ui: &mut egui::Ui) -> (bool, bool) {
    let rect = ui.max_rect();
    let hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 68.0, rect.top() + 3.0),
        egui::pos2(rect.right() - 6.0, rect.top() + 29.0),
    );
    let drag_response = ui.interact(
        rect,
        ui.make_persistent_id("weather_location_drag"),
        egui::Sense::drag(),
    );
    if drag_response.drag_started() {
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
        ui.make_persistent_id("weather_location_minimize"),
        egui::Sense::click(),
    );
    let close = ui.interact(
        close_rect,
        ui.make_persistent_id("weather_location_close"),
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
