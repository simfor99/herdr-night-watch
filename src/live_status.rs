use crate::{
    backend::{self, AgentSummary, CompletionAction, WatchStatus},
    language::Language,
    log_viewer,
    media::{self, MediaCommand, MediaSnapshot},
    system_metrics::{self, SystemMetrics},
    taskbar,
    weather::{self, WeatherLocation, WeatherReading},
    weather_location, window_chrome, window_settings,
};
use anyhow::{Context, Result};
use eframe::egui;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{BOOL, CloseHandle, GetLastError, HANDLE, HWND, LPARAM};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    SW_RESTORE, SetForegroundWindow, ShowWindow,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// WGPU can take noticeably longer to initialize after a Windows reboot while
// the graphics driver and desktop compositor are still waking up. Four
// seconds caused the tray opener to kill a healthy process during startup.
const LIVE_WINDOW_START_TIMEOUT: Duration = Duration::from_secs(30);
const LIVE_WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIVE_WINDOW_START_ATTEMPTS: usize = 2;
const LIVE_WINDOW_RETRY_DELAY: Duration = Duration::from_secs(2);
const LIVE_INSTANCE_MUTEX: &str = "Local\\HerdrNachtwaechter.LiveStatus";
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
const MOON_SLOT_WIDTH: f32 = 77.0;
const MOON_ICON_DIAMETER: f32 = 59.5;
const MOON_RIGHT_INSET: f32 = 34.0;
const KPI_PANEL_WIDTH: f32 = 264.0;

pub fn open() -> Result<()> {
    if let Some(hwnd) = find_live_window() {
        apply_taskbar_visibility(hwnd);
        activate_live_window(hwnd);
        return Ok(());
    }

    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    let child = spawn_live_status_process(&executable)?;

    thread::spawn(move || {
        let mut child = child;
        for attempt in 1..=LIVE_WINDOW_START_ATTEMPTS {
            let started_at = Instant::now();
            loop {
                if let Some(hwnd) = find_live_window_for_pid(child.id()) {
                    apply_taskbar_visibility(hwnd);
                    activate_live_window(hwnd);
                    return;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let detail = format!(
                            "Live-Status-Prozess wurde beendet, bevor ein Fenster sichtbar wurde ({status})"
                        );
                        if attempt == LIVE_WINDOW_START_ATTEMPTS {
                            record_open_failure(&detail);
                            return;
                        }
                        record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                        break;
                    }
                    Ok(None) if started_at.elapsed() >= LIVE_WINDOW_START_TIMEOUT => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let detail = "Live-Status-Prozess läuft, aber nach 30 Sekunden wurde kein Fenster gefunden";
                        if attempt == LIVE_WINDOW_START_ATTEMPTS {
                            record_open_failure(detail);
                            return;
                        }
                        record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let detail = format!(
                            "Live-Status-Fenster wurde nicht sichtbar; Prozessstatus konnte nicht geprüft werden: {error}"
                        );
                        if attempt == LIVE_WINDOW_START_ATTEMPTS {
                            record_open_failure(&detail);
                            return;
                        }
                        record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                        break;
                    }
                }
                thread::sleep(LIVE_WINDOW_POLL_INTERVAL);
            }
            thread::sleep(LIVE_WINDOW_RETRY_DELAY);
            child = match spawn_live_status_process(&executable) {
                Ok(child) => child,
                Err(error) => {
                    record_open_failure(&format!(
                        "Live-Status-Prozess konnte beim Wiederholungsversuch nicht gestartet werden: {error}"
                    ));
                    return;
                }
            };
        }
    });
    Ok(())
}

fn spawn_live_status_process(executable: &Path) -> Result<Child> {
    Command::new(executable)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--live-status")
        .spawn()
        .context("Live-Status-Fenster konnte nicht gestartet werden")
}

fn find_live_window() -> Option<HWND> {
    let mut found = None;
    unsafe {
        let _ = EnumWindows(
            Some(find_live_window_callback),
            &mut found as *mut _ as LPARAM,
        );
    }
    found
}

fn find_live_window_for_pid(pid: u32) -> Option<HWND> {
    let mut search = LiveWindowSearch {
        target_pid: pid,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_live_window_for_pid_callback),
            &mut search as *mut _ as LPARAM,
        );
    }
    search.found
}

struct LiveWindowSearch {
    target_pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn find_live_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let found = &mut *(lparam as *mut Option<HWND>);
        if found.is_some() || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return 1;
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let title = String::from_utf16_lossy(&title[..copied as usize]);
        if title == "Herdr-Nachtwächter - Live-Status" || title == "Herdr Night Watch - Live Status"
        {
            *found = Some(hwnd);
            return 0;
        }
    }
    1
}

unsafe extern "system" fn find_live_window_for_pid_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam as *mut LiveWindowSearch);
        if search.found.is_some() {
            return 1;
        }
        // eframe/winit creates a 4x4 event-target window before the actual
        // Live-Status window is shown. The real window can still be marked
        // hidden while it is being prepared, so match its title rather than
        // requiring IsWindowVisible here. activate_live_window restores it.
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return 1;
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let title = String::from_utf16_lossy(&title[..copied as usize]);
        if title != "Herdr-Nachtwächter - Live-Status" && title != "Herdr Night Watch - Live Status"
        {
            return 1;
        }
        let mut window_pid = 0;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if window_pid == search.target_pid {
            search.found = Some(hwnd);
            return 0;
        }
    }
    1
}

fn activate_live_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn apply_taskbar_visibility(hwnd: HWND) {
    let _ = taskbar::set_visible(hwnd, window_settings::live_status_in_taskbar());
}

pub fn apply_taskbar_setting() -> Result<()> {
    if let Some(hwnd) = find_live_window()
        && !taskbar::set_visible(hwnd, window_settings::live_status_in_taskbar())
    {
        anyhow::bail!("Windows konnte die Taskleistenanzeige nicht ändern");
    }
    Ok(())
}

fn record_open_failure(detail: &str) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(directory) = executable.parent() else {
        return;
    };
    let log_directory = directory.join("logs");
    if fs::create_dir_all(&log_directory).is_err() {
        return;
    }
    let path = log_directory.join("ui-errors.log");
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(
        file,
        "{:?};live_status_open_failed;{}",
        SystemTime::now(),
        detail.replace(['\r', '\n', ';'], " ")
    );
}

pub fn run() -> Result<()> {
    let instance_mutex = match acquire_live_instance()? {
        Some(handle) => handle,
        None => {
            if let Some(hwnd) = find_live_window() {
                apply_taskbar_visibility(hwnd);
                activate_live_window(hwnd);
            }
            return Ok(());
        }
    };

    let result = run_window();
    if let Err(error) = &result {
        record_open_failure(&format!("Live-Status-Prozessfehler: {error}"));
    }
    unsafe {
        let _ = CloseHandle(instance_mutex);
    }
    result
}

fn acquire_live_instance() -> Result<Option<HANDLE>> {
    let name: Vec<u16> = LIVE_INSTANCE_MUTEX
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow::anyhow!(
            "Live-Status-Sperre konnte nicht erstellt werden"
        ));
    }
    if unsafe { GetLastError() } == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        Ok(None)
    } else {
        Ok(Some(handle))
    }
}

fn run_window() -> Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([393.0, 190.0])
        .with_min_inner_size([383.0, 180.0])
        .with_decorations(false)
        .with_window_level(window_chrome::window_level(
            window_settings::WindowLevel::current(),
        ))
        .with_title(match Language::current() {
            Language::German => "Herdr-Nachtwächter - Live-Status",
            Language::English => "Herdr Night Watch - Live Status",
        });
    if let Some(position) = window_settings::live_status_position() {
        viewport = viewport.with_position(position);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
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
    media_command_tx: Sender<MediaCommand>,
    media_rx: Receiver<Result<Option<MediaSnapshot>, String>>,
    media_snapshot: Option<MediaSnapshot>,
    weather_rx: Receiver<Result<WeatherReading, String>>,
    weather_tx: Sender<Result<WeatherReading, String>>,
    weather_location: WeatherLocation,
    weather_reading: Option<WeatherReading>,
    weather_checking: bool,
    last_weather_fetch: Instant,
    last_weather_location_check: Instant,
    opacity: Option<u8>,
    window_level: window_settings::WindowLevel,
    taskbar_visible: Option<bool>,
    window_drag_started: bool,
    last_saved_position: Option<[f32; 2]>,
}

impl LiveStatusApp {
    fn new() -> Self {
        let (status_tx, status_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (metrics_tx, metrics_rx) = mpsc::channel();
        let (media_command_tx, media_rx) = media::spawn_worker();
        let (weather_tx, weather_rx) = mpsc::channel();
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
            media_command_tx,
            media_rx,
            media_snapshot: None,
            weather_rx,
            weather_tx,
            weather_location: weather::current_location(),
            weather_reading: None,
            weather_checking: false,
            last_weather_fetch: Instant::now() - Duration::from_secs(601),
            last_weather_location_check: Instant::now() - Duration::from_secs(6),
            opacity: None,
            window_level: window_settings::WindowLevel::current(),
            taskbar_visible: None,
            window_drag_started: false,
            last_saved_position: window_settings::live_status_position(),
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

    fn collect_media(&mut self) {
        while let Ok(result) = self.media_rx.try_recv() {
            self.media_snapshot = result.ok().flatten();
        }
    }

    fn refresh_weather(&mut self) {
        if self.last_weather_location_check.elapsed() >= Duration::from_secs(5) {
            self.last_weather_location_check = Instant::now();
            let location = weather::current_location();
            if location != self.weather_location {
                self.weather_location = location;
                self.weather_reading = None;
                self.last_weather_fetch = Instant::now() - Duration::from_secs(601);
            }
        }
        if self.weather_checking || self.last_weather_fetch.elapsed() < Duration::from_secs(600) {
            return;
        }
        self.weather_checking = true;
        self.last_weather_fetch = Instant::now();
        let sender = self.weather_tx.clone();
        let location = self.weather_location.clone();
        thread::spawn(move || {
            let _ =
                sender.send(weather::fetch_current(location).map_err(|error| error.to_string()));
        });
    }

    fn collect_weather(&mut self) {
        while let Ok(result) = self.weather_rx.try_recv() {
            self.weather_checking = false;
            if let Ok(reading) = result {
                self.weather_location = reading.location.clone();
                self.weather_reading = Some(reading);
            }
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
            window_chrome::apply_window_opacity(current_opacity, live_title(self.language));
            self.opacity = Some(current_opacity);
        }
        let current_window_level = window_settings::WindowLevel::current();
        if current_window_level != self.window_level {
            self.window_level = current_window_level;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                    window_chrome::window_level(current_window_level),
                ));
        }
        let current_taskbar_visibility = window_settings::live_status_in_taskbar();
        if self.taskbar_visible != Some(current_taskbar_visibility)
            && let Some(hwnd) = find_live_window()
            && taskbar::set_visible(hwnd, current_taskbar_visibility)
        {
            self.taskbar_visible = Some(current_taskbar_visibility);
        }
        self.collect_results();
        self.collect_actions();
        self.collect_metrics();
        self.collect_media();
        self.collect_weather();
        self.refresh();
        self.refresh_weather();
        window_chrome::paint_gradient(ui.painter(), ui.max_rect(), BG_TOP, BG_BOTTOM);
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
                        .send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                            window_chrome::window_level(next_level),
                        ));
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
            .inner_margin(egui::Margin {
                left: 20,
                right: 1,
                top: 14,
                bottom: 14,
            })
            .show(ui, |ui| {
                let moon = moon_view(
                    &self.status,
                    self.pending_action,
                    self.error.as_deref(),
                    self.language,
                    self.weather_reading.as_ref(),
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
                                        .desired_width(50.0)
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
                                // Keep the KPI card geometrically stable across
                                // languages; English labels are fitted inside
                                // the German-sized card instead of widening it.
                                ui.set_width(KPI_PANEL_WIDTH - 24.0);
                                let agents = agents_for(&self.status);
                                if agents.available {
                                    let old_spacing = ui.spacing().item_spacing.x;
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.horizontal(|ui| {
                                        metric(ui, self.language.text("Erkannt", "Detected"), agents.total, TEXT);
                                        divider(ui);
                                        metric(ui, self.language.text("Arbeitet", "Working"), agents.working, GREEN);
                                        divider(ui);
                                        metric(ui, self.language.text("Bereit", "Ready"), agents.idle, ACCENT);
                                        divider(ui);
                                        metric(ui, self.language.text("Fertig", "Finished"), agents.done, GRAY);
                                    });
                                    ui.spacing_mut().item_spacing.x = old_spacing;
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
                            window_chrome::glass_sheen(ui.painter(), panel.response.rect);
                        });
                        let moon_space = (ui.max_rect().right()
                            - MOON_RIGHT_INSET
                            - MOON_SLOT_WIDTH
                            - ui.cursor().left())
                            .max(0.0);
                        ui.add_space(moon_space);
                        ui.allocate_ui_with_layout(
                            egui::vec2(MOON_SLOT_WIDTH, 88.0),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                ui.add_space(30.0);
                                let response = moon_icon(
                                        ui,
                                        moon.color,
                                        MOON_ICON_DIAMETER,
                                        gradient_rect,
                                        moon.temperature_c,
                                        moon.phase,
                                    )
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
                            },
                        );
                    },
                );
                ui.add_space(6.0);
                let metrics_text_right = system_metrics_row(ui, self.metrics, self.language);
                if let Some(media) = &self.media_snapshot {
                    ui.add_space(4.0);
                    if let Some(position) = media_info_row(ui, media, metrics_text_right) {
                        let _ = self.media_command_tx.send(MediaCommand::Seek(position));
                    }
                }
                if weather_control_overlay(ui, &self.weather_reading, self.language)
                    && let Err(error) = weather_location::open()
                {
                    self.error = Some(error.to_string());
                }
        });
        self.show_toast(ui.ctx());
        handle_window_drag(self, ui);
        persist_window_position(self, ui.ctx());
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }
}

fn persist_window_position(app: &mut LiveStatusApp, ctx: &egui::Context) {
    if ctx.input(|input| input.pointer.primary_down()) {
        return;
    }
    let position = ctx.input(|input| {
        input
            .viewport()
            .outer_rect
            .map(|rect| [rect.min.x.round(), rect.min.y.round()])
    });
    if position != app.last_saved_position
        && let Some(position) = position
        && window_settings::set_live_status_position(position).is_ok()
    {
        app.last_saved_position = Some(position);
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
        egui::pos2(rect.right() - 200.0, rect.top() + 28.0),
        egui::pos2(rect.right() - 20.0, rect.top() + 116.0),
    );
    let control_hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 138.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 3.0, rect.top() + 30.0),
    );
    let weather_control = weather_control_rect(rect);
    let is_excluded = |position: egui::Pos2| {
        completion_switch.contains(position)
            || warning_seconds.contains(position)
            || moon.contains(position)
            || control_hood.contains(position)
            || weather_control.contains(position)
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
    window_chrome::glass_sheen(ui.painter(), hood_rect);
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
    temperature_c: Option<f64>,
    phase: f64,
}

fn moon_view(
    status: &WatchStatus,
    pending_action: Option<NightAction>,
    error: Option<&str>,
    language: Language,
    weather_reading: Option<&WeatherReading>,
) -> MoonView {
    let temperature_c = weather_reading.map(|reading| reading.temperature_c);
    let phase = weather_reading
        .map(|reading| reading.moon_phase)
        .unwrap_or_else(weather::estimated_moon_phase);
    let phase_suffix = format!(
        "\n{}: {}",
        language.text("Mondphase", "Moon phase"),
        moon_phase_label(phase, language),
    );
    let weather_suffix = weather_reading
        .map(|reading| {
            format!(
                "\n{}: {:.0} °C\n{}: {}",
                reading.location.name,
                reading.temperature_c,
                language.text("Messzeit", "Observed"),
                reading.observed_at,
            )
        })
        .unwrap_or_else(|| {
            language
                .text(
                    "\nTemperatur momentan nicht verfügbar",
                    "\nTemperature currently unavailable",
                )
                .into()
        });
    if matches!(pending_action, Some(NightAction::Start)) {
        return MoonView {
            color: GREEN,
            tooltip: format!(
                "{}{}{}",
                language.text(
                    "Nachtmodus wird aktiviert …\nBitte kurz warten.",
                    "Night mode is being enabled …\nPlease wait.",
                ),
                phase_suffix,
                weather_suffix,
            ),
            temperature_c,
            phase,
        };
    }
    if matches!(pending_action, Some(NightAction::Stop)) {
        return MoonView {
            color: GRAY,
            tooltip: format!(
                "{}{}{}",
                language.text(
                    "Nachtmodus wird deaktiviert …\nBitte kurz warten.",
                    "Night mode is being disabled …\nPlease wait.",
                ),
                phase_suffix,
                weather_suffix,
            ),
            temperature_c,
            phase,
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
        WatchStatus::Watching { demo, .. } if *demo => (
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
        format!("{state}\nAktion fehlgeschlagen: {error}\n{action}{phase_suffix}{weather_suffix}")
    } else {
        format!("{state}\n{action}{phase_suffix}{weather_suffix}")
    };
    MoonView {
        color,
        tooltip,
        temperature_c,
        phase,
    }
}

fn moon_phase_label(phase: f64, language: Language) -> &'static str {
    let phase = phase.rem_euclid(1.0);
    match phase {
        value if !(0.0625..0.9375).contains(&value) => language.text("Neumond", "New moon"),
        value if value < 0.1875 => language.text("Zunehmende Sichel", "Waxing crescent"),
        value if value < 0.3125 => language.text("Erstes Viertel", "First quarter"),
        value if value < 0.4375 => language.text("Zunehmender Mond", "Waxing gibbous"),
        value if value < 0.5625 => language.text("Vollmond", "Full moon"),
        value if value < 0.6875 => language.text("Abnehmender Mond", "Waning gibbous"),
        value if value < 0.8125 => language.text("Letztes Viertel", "Last quarter"),
        _ => language.text("Abnehmende Sichel", "Waning crescent"),
    }
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
    window_chrome::glass_sheen(painter, rect);

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
    temperature_c: Option<f64>,
    phase: f64,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click());
    let center = rect.center();
    let radius = diameter / 2.0;
    let painter = ui.painter();
    let phase = phase.rem_euclid(1.0);
    let halo_outer = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 18);
    let halo_inner = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32);
    painter.circle_filled(center, radius * 1.28, halo_outer);
    painter.circle_filled(center, radius * 1.14, halo_inner);
    let illumination = (1.0 - (std::f32::consts::TAU * phase as f32).cos()) * 0.5;
    let dark_side = if phase < 0.5 { 1.0 } else { -1.0 };
    let separation = moon_disc_separation(illumination) * radius;
    let dark_center = egui::pos2(center.x + separation * dark_side, center.y);
    painter.circle_filled(center, radius, color);
    paint_moon_shadow(
        painter,
        center,
        dark_center,
        radius,
        gradient_color_at(gradient_rect, dark_center),
    );
    if let Some(temperature_c) = temperature_c {
        paint_moon_temperature(
            painter,
            rect,
            center,
            dark_side,
            separation,
            illumination,
            temperature_c,
        );
    }
    response
}

fn paint_moon_shadow(
    painter: &egui::Painter,
    moon_center: egui::Pos2,
    shadow_center: egui::Pos2,
    radius: f32,
    color: egui::Color32,
) {
    let delta = shadow_center - moon_center;
    let separation = delta.length();
    if separation <= f32::EPSILON {
        painter.circle_filled(moon_center, radius, color);
        return;
    }
    if separation >= radius * 2.0 {
        return;
    }

    // The shadow is the intersection of two equal circles. Drawing that
    // lens directly keeps the unlit disc inside the actual moon boundary.
    let direction = delta.y.atan2(delta.x);
    let half_angle = (separation / (radius * 2.0)).clamp(-1.0, 1.0).acos();
    let arc_steps = 24;
    let mut points = Vec::with_capacity(arc_steps * 2 + 2);
    for step in 0..=arc_steps {
        let t = step as f32 / arc_steps as f32;
        let angle = direction - half_angle + 2.0 * half_angle * t;
        points.push(egui::pos2(
            moon_center.x + radius * angle.cos(),
            moon_center.y + radius * angle.sin(),
        ));
    }
    for step in 0..=arc_steps {
        let t = step as f32 / arc_steps as f32;
        let angle = direction + std::f32::consts::PI - half_angle + 2.0 * half_angle * t;
        points.push(egui::pos2(
            shadow_center.x + radius * angle.cos(),
            shadow_center.y + radius * angle.sin(),
        ));
    }
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

fn paint_moon_temperature(
    painter: &egui::Painter,
    moon_rect: egui::Rect,
    center: egui::Pos2,
    dark_side: f32,
    separation: f32,
    illumination: f32,
    temperature_c: f64,
) {
    let position = egui::Align2::CENTER_CENTER;
    let radius = moon_rect.width() * 0.5;
    let text = format!("{temperature_c:.0}°C");
    let dark_text = egui::Color32::from_rgb(24, 31, 47);
    let light_text = egui::Color32::from_rgb(247, 241, 229);

    if illumination <= 0.12 {
        painter.text(
            center,
            position,
            text,
            egui::FontId::proportional(15.0),
            light_text,
        );
        return;
    }

    if illumination >= 0.88 {
        painter.text(
            center,
            position,
            text,
            egui::FontId::proportional(15.0),
            dark_text,
        );
        return;
    }

    // For crescents and gibbous moons, keep the complete label in the dark
    // overlap region. Its centre on the moon's horizontal axis is half-way
    // between the two disc centres, which keeps all glyphs on the dark side.
    let near_half_moon = (0.34..=0.88).contains(&illumination);
    if !near_half_moon {
        let dark_center = egui::pos2(center.x + dark_side * separation * 0.5, center.y);
        painter.text(
            dark_center,
            position,
            text,
            egui::FontId::proportional(15.0),
            light_text,
        );
        return;
    }

    // For a gibbous/half-moon view keep the label centred and colour each
    // side independently at the actual terminator, not at an arbitrary
    // midpoint between the two circle centres.
    let split_x = center.x + dark_side * (separation - radius);
    let left_clip =
        egui::Rect::from_min_max(moon_rect.min, egui::pos2(split_x, moon_rect.bottom()));
    let right_clip = egui::Rect::from_min_max(egui::pos2(split_x, moon_rect.top()), moon_rect.max);
    let lit_is_left = dark_side > 0.0;
    let lit_painter = if lit_is_left {
        painter.with_clip_rect(left_clip)
    } else {
        painter.with_clip_rect(right_clip)
    };
    let dark_painter = if lit_is_left {
        painter.with_clip_rect(right_clip)
    } else {
        painter.with_clip_rect(left_clip)
    };
    let font = egui::FontId::proportional(13.0);
    lit_painter.text(center, position, text.clone(), font.clone(), dark_text);
    dark_painter.text(center, position, text, font, light_text);
}

fn moon_disc_separation(illumination: f32) -> f32 {
    let target_overlap = 1.0 - illumination.clamp(0.0, 1.0);
    let mut low = 0.0;
    let mut high = 2.0;
    for _ in 0..24 {
        let middle = (low + high) * 0.5;
        if circle_overlap_fraction(middle) > target_overlap {
            low = middle;
        } else {
            high = middle;
        }
    }
    (low + high) * 0.5
}

fn circle_overlap_fraction(separation: f32) -> f32 {
    if separation <= 0.0 {
        return 1.0;
    }
    if separation >= 2.0 {
        return 0.0;
    }
    let root = (4.0 - separation * separation).sqrt();
    (2.0 * (separation / 2.0).acos() - 0.5 * separation * root) / std::f32::consts::PI
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

fn system_metrics_row(ui: &mut egui::Ui, metrics: SystemMetrics, language: Language) -> f32 {
    let available = ui.available_width();
    let spacing = 7.0;
    let item_width = ((available - spacing * 4.0) / 5.0).max(52.0);
    let power_value = metrics.gpu_watts.map(|value| format!("{value:>3}W"));
    let old_spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = spacing;
    let power_rect = ui
        .horizontal(|ui| {
            system_metric_badge(
                ui,
                item_width,
                MetricIcon::Cpu,
                "CPU",
                metrics.cpu_percent.map(|value| format!("{value:>2}%")),
                metric_color(metrics.cpu_percent),
                language,
                false,
            );
            system_metric_badge(
                ui,
                item_width,
                MetricIcon::Ram,
                "RAM",
                metrics.ram_percent.map(|value| format!("{value:>2}%")),
                metric_color(metrics.ram_percent),
                language,
                false,
            );
            system_metric_badge(
                ui,
                item_width,
                MetricIcon::Gpu,
                "GPU",
                metrics.gpu_percent.map(|value| format!("{value:>2}%")),
                metric_color(metrics.gpu_percent),
                language,
                false,
            );
            system_metric_badge(
                ui,
                item_width,
                MetricIcon::Vram,
                "VRAM",
                metrics.vram_percent.map(|value| format!("{value:>2}%")),
                metric_color(metrics.vram_percent),
                language,
                false,
            );
            system_metric_badge(
                ui,
                item_width,
                MetricIcon::Power,
                "",
                power_value.clone(),
                metric_color(metrics.gpu_power_percent),
                language,
                true,
            )
        })
        .inner;
    ui.spacing_mut().item_spacing.x = old_spacing;
    let power_text = power_value.unwrap_or_else(|| "—".into());
    let power_text_width = ui
        .painter()
        .layout_no_wrap(
            power_text,
            egui::FontId::proportional(11.0),
            metric_color(metrics.gpu_power_percent),
        )
        .size()
        .x;
    power_rect.center().x - 5.0 + power_text_width / 2.0
}

fn media_info_row(
    ui: &mut egui::Ui,
    media: &MediaSnapshot,
    metrics_text_right: f32,
) -> Option<i64> {
    let (artist_color, _, title_color, _) = media_colors(media);
    let artist = if media.artist.trim().is_empty() {
        "Unbekannter Interpret".to_string()
    } else {
        media.artist.clone()
    };
    let timeline_reserve = 110.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let text_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(
            (rect.right() - timeline_reserve - 6.0).max(rect.left()),
            rect.bottom(),
        ),
    );
    let max_chars = (text_rect.width() / 6.2).floor().max(18.0) as usize;
    let artist_part = truncate_media_text(&artist, (max_chars as f32 * 0.68) as usize);
    let title_budget = max_chars.saturating_sub(artist_part.chars().count()).max(8);
    let title_part = truncate_media_text(&media.title, title_budget);
    let painter = ui.painter();
    let artist_galley =
        painter.layout_no_wrap(artist_part, egui::FontId::proportional(10.5), artist_color);
    let title_galley =
        painter.layout_no_wrap(title_part, egui::FontId::proportional(10.5), title_color);
    let artist_panel = egui::Rect::from_min_size(
        text_rect.min,
        egui::vec2(artist_galley.size().x + 8.0, text_rect.height()),
    );
    let title_panel = egui::Rect::from_min_size(
        egui::pos2(artist_panel.right() + 7.0, text_rect.top()),
        egui::vec2(
            (title_galley.size().x + 8.0)
                .min((text_rect.right() - artist_panel.right() - 7.0).max(0.0)),
            text_rect.height(),
        ),
    );
    draw_media_panel(
        painter,
        artist_panel,
        artist_color,
        artist_color,
        12,
        42,
        52,
    );
    draw_media_panel(painter, title_panel, title_color, title_color, 11, 38, 50);
    painter.galley(
        egui::pos2(
            artist_panel.left() + 4.0,
            text_rect.center().y - artist_galley.size().y / 2.0,
        ),
        artist_galley,
        artist_color,
    );
    painter.galley(
        egui::pos2(
            title_panel.left() + 4.0,
            text_rect.center().y - title_galley.size().y / 2.0,
        ),
        title_galley,
        title_color,
    );

    let timeline_left = (title_panel.right() + 6.0).min(rect.right() - timeline_reserve);
    let timeline_right = (metrics_text_right + 5.0).clamp(timeline_left, rect.right());
    let timeline_rect = egui::Rect::from_min_max(
        egui::pos2(timeline_left, rect.top()),
        egui::pos2(timeline_right, rect.bottom()),
    );
    let track_left = timeline_rect.left() + 5.0;
    let track_right = timeline_rect.right() - 5.0;
    let track_y = timeline_rect.center().y;
    let track_width = (track_right - track_left).max(1.0);
    let duration = media.end_100ns.saturating_sub(media.start_100ns);
    let progress = if media.seek_enabled && duration > 0 {
        ((media.position_100ns - media.start_100ns) as f32 / duration as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let timeline_response = ui.interact(
        timeline_rect,
        ui.make_persistent_id("media_playback_timeline"),
        egui::Sense::click(),
    );
    const DOT_COUNT: usize = 72;
    const DOT_STACK: [f32; 6] = [-7.5, -4.5, -1.5, 1.5, 4.5, 7.5];
    let completed_dots = if media.seek_enabled && duration > 0 {
        ((DOT_COUNT - 1) as f32 * progress).round() as usize
    } else {
        0
    };
    let hover_progress = if timeline_response.hovered() && media.seek_enabled && duration > 0 {
        timeline_response
            .hover_pos()
            .map(|position| ((position.x - track_left) / track_width).clamp(0.0, 1.0))
    } else {
        None
    };
    let hover_dots = hover_progress.map(|value| ((DOT_COUNT - 1) as f32 * value).round() as usize);
    // The played portion transitions from the artist pill's color to the
    // title pill's color. Lower alpha keeps the gradient pleasantly quiet
    // against the dark background while preserving the visual distinction.
    let preview_color = egui::Color32::from_rgba_unmultiplied(
        title_color.r(),
        title_color.g(),
        title_color.b(),
        180,
    );
    let remaining_color = egui::Color32::from_rgb(72, 78, 92);
    for index in 0..DOT_COUNT {
        let fraction = index as f32 / (DOT_COUNT - 1) as f32;
        let in_preview_range = hover_dots.is_some_and(|hover| {
            (hover > completed_dots && index > completed_dots && index <= hover)
                || (hover < completed_dots && index >= hover && index < completed_dots)
        });
        let completed_color = blend_media_colors(artist_color, title_color, fraction, 165);
        let color = if in_preview_range {
            preview_color
        } else if media.seek_enabled && index <= completed_dots {
            completed_color
        } else {
            remaining_color
        };
        let radius = if hover_dots == Some(index) { 1.65 } else { 1.1 };
        for offset in DOT_STACK {
            painter.circle_filled(
                egui::pos2(track_left + track_width * fraction, track_y + offset),
                radius,
                color,
            );
        }
    }
    if !timeline_response.clicked() || !media.seek_enabled || duration <= 0 {
        return None;
    }
    let pointer_x = timeline_response.interact_pointer_pos()?.x;
    let ratio = ((pointer_x - track_left) / track_width).clamp(0.0, 1.0);
    Some(media.start_100ns + (duration as f32 * ratio) as i64)
}

fn blend_media_colors(
    start: egui::Color32,
    end: egui::Color32,
    amount: f32,
    alpha: u8,
) -> egui::Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let blend = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * amount).round() as u8
    };
    egui::Color32::from_rgba_unmultiplied(
        blend(start.r(), end.r()),
        blend(start.g(), end.g()),
        blend(start.b(), end.b()),
        alpha,
    )
}

fn draw_media_panel(
    painter: &egui::Painter,
    rect: egui::Rect,
    text_color: egui::Color32,
    background: egui::Color32,
    glow_alpha: u8,
    fill_alpha: u8,
    stroke_alpha: u8,
) {
    painter.rect_stroke(
        rect.expand(1.5),
        egui::CornerRadius::same(6),
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(
                text_color.r(),
                text_color.g(),
                text_color.b(),
                glow_alpha,
            ),
        ),
        egui::StrokeKind::Outside,
    );
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(5),
        egui::Color32::from_rgba_unmultiplied(
            background.r(),
            background.g(),
            background.b(),
            fill_alpha,
        ),
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(5),
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(
                text_color.r(),
                text_color.g(),
                text_color.b(),
                stroke_alpha,
            ),
        ),
        egui::StrokeKind::Inside,
    );
}

fn truncate_media_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn media_colors(
    media: &MediaSnapshot,
) -> (egui::Color32, egui::Color32, egui::Color32, egui::Color32) {
    let mut hash = 2_166_136_261u32;
    for byte in media.title.bytes().chain(media.artist.bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    let base_hue = (hash % 360) as f32;
    let artist_color = hsv_color(base_hue, 0.34, 0.98);
    let artist_background = hsv_color(base_hue + 180.0, 0.60, 0.46);
    let title_color = hsv_color(base_hue + 52.0, 0.34, 0.98);
    let title_background = hsv_color(base_hue + 232.0, 0.60, 0.44);
    (
        artist_color,
        artist_background,
        title_color,
        title_background,
    )
}

fn hsv_color(hue: f32, saturation: f32, value: f32) -> egui::Color32 {
    let hue = (hue.rem_euclid(360.0)) / 60.0;
    let chroma = value * saturation;
    let x = chroma * (1.0 - ((hue % 2.0) - 1.0).abs());
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let match_value = value - chroma;
    egui::Color32::from_rgb(
        ((red + match_value) * 255.0).round() as u8,
        ((green + match_value) * 255.0).round() as u8,
        ((blue + match_value) * 255.0).round() as u8,
    )
}

fn weather_control_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(rect.right() - 12.0, rect.bottom() - 10.0),
        egui::vec2(22.0, 18.0),
    )
}

fn weather_control_overlay(
    ui: &mut egui::Ui,
    reading: &Option<WeatherReading>,
    language: Language,
) -> bool {
    let rect = weather_control_rect(ui.max_rect());
    let response = ui.interact(
        rect,
        ui.make_persistent_id("live_weather_location"),
        egui::Sense::click(),
    );
    let visible = response.hovered();
    if visible {
        ui.painter().rect_filled(
            rect.expand(2.0),
            egui::CornerRadius::same(6),
            egui::Color32::from_rgba_unmultiplied(52, 54, 66, 220),
        );
        let color = if reading.is_some() { ACCENT } else { GRAY };
        let center = rect.center();
        ui.painter()
            .circle_stroke(center, 4.0, egui::Stroke::new(1.2, color));
        ui.painter().line_segment(
            [
                egui::pos2(center.x, center.y - 7.0),
                egui::pos2(center.x, center.y + 7.0),
            ],
            egui::Stroke::new(1.2, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x - 2.0, center.y - 7.0),
                egui::pos2(center.x + 2.0, center.y - 7.0),
            ],
            egui::Stroke::new(1.2, color),
        );
    }
    response
        .on_hover_text(match reading {
            Some(reading) => format!(
                "{}\n{}: {:.0} °C\n{}",
                language.text("Wetterort ändern", "Change weather location"),
                reading.location.name,
                reading.temperature_c,
                language.text("Klicken zum Suchen", "Click to search"),
            ),
            None => language
                .text(
                    "Wetterort festlegen\nKlicken zum Suchen",
                    "Set weather location\nClick to search",
                )
                .into(),
        })
        .clicked()
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
    center_content: bool,
) -> egui::Rect {
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
    let has_value = value.is_some();
    let value = value.unwrap_or_else(|| "—".into());
    let text = if label.is_empty() {
        value.clone()
    } else {
        format!("{label} {value}")
    };
    let text_width = ui
        .painter()
        .layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(11.0),
            if has_value { color } else { GRAY },
        )
        .size()
        .x;
    let text_left = if center_content {
        rect.center().x - 5.0 - text_width / 2.0
    } else {
        rect.left() + 17.0
    };
    let icon_x = if center_content {
        text_left - 10.0
    } else {
        rect.left() + 7.0
    };
    draw_metric_icon(
        ui.painter(),
        icon,
        egui::pos2(icon_x, rect.center().y),
        color,
    );
    ui.painter().text(
        egui::pos2(text_left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.0),
        if has_value { color } else { GRAY },
    );
    let _ = response.on_hover_text(tooltip);
    rect
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
