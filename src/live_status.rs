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
use std::sync::{
    Mutex,
    mpsc::{self, Receiver, Sender},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{BOOL, CloseHandle, GetLastError, HANDLE, HWND, LPARAM, RECT};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, GetExitCodeProcess, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SW_RESTORE,
    SW_SHOW, SetForegroundWindow, ShowWindow, WM_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// WGPU can take noticeably longer to initialize after a Windows reboot while
// the graphics driver and desktop compositor are still waking up. Four
// seconds caused the tray opener to kill a healthy process during startup.
const LIVE_WINDOW_START_TIMEOUT: Duration = Duration::from_secs(30);
const LIVE_WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIVE_WINDOW_START_ATTEMPTS: usize = 2;
const LIVE_WINDOW_RETRY_DELAY: Duration = Duration::from_secs(2);
const PRIMARY_TRAY_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const PRIMARY_TRAY_DISCOVERY_INTERVAL: Duration = Duration::from_millis(100);
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
const DESIGN_WIDTH: f32 = 393.0;
const DESIGN_HEIGHT: f32 = 190.0;
const RESIZE_GRIP_SIZE: f32 = 16.0;
const LIVE_WINDOW_MIN_READY_WIDTH: i32 = 80;
const LIVE_WINDOW_MIN_READY_HEIGHT: i32 = 40;
const STILL_ACTIVE: u32 = 259;
const OPEN_SPAWN_DEBOUNCE: Duration = Duration::from_millis(750);
const LIVE_WINDOW_CLOSE_TIMEOUT: Duration = Duration::from_millis(1500);
const OWNER_PID_PREFIX: &str = "--owner-pid=";

struct OpenAttempt {
    started_at: Instant,
    child_pid: Option<u32>,
}

static OPEN_ATTEMPT: Mutex<Option<OpenAttempt>> = Mutex::new(None);
const MOON_SLOT_WIDTH: f32 = 77.0;
const MOON_ICON_DIAMETER: f32 = 59.5;
const MOON_RIGHT_INSET: f32 = 34.0;
const KPI_PANEL_WIDTH: f32 = 264.0;

pub fn open() -> Result<()> {
    open_with_owner(Some(current_pid()))
}

/// Open the live window from a secondary tray invocation while keeping the
/// long-lived tray process as its owner. A second tray process only exists to
/// hand the request to the primary instance and exits shortly afterwards.
pub fn open_from_secondary_instance() -> Result<()> {
    // During Windows logon the primary tray process can exist before its
    // helper window is registered. Wait briefly so the live window is owned
    // by the long-lived tray process instead of the short-lived secondary
    // invocation.
    open_with_owner(find_primary_tray_pid_with_retry())
}

fn open_with_owner(owner_pid: Option<u32>) -> Result<()> {
    if let Some(hwnd) = find_existing_live_window() {
        apply_taskbar_visibility(hwnd);
        activate_live_window(hwnd);
        return Ok(());
    }

    if !begin_open_spawn() {
        return Ok(());
    }

    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    let child = match spawn_live_status_process(&executable, owner_pid) {
        Ok(child) => child,
        Err(error) => {
            finish_open_spawn();
            return Err(error);
        }
    };
    remember_open_child(child.id());

    thread::spawn(move || {
        let mut child = child;
        for attempt in 1..=LIVE_WINDOW_START_ATTEMPTS {
            remember_open_child(child.id());
            let started_at = Instant::now();
            loop {
                if let Some(hwnd) = find_live_window_for_pid(child.id()) {
                    apply_taskbar_visibility(hwnd);
                    activate_live_window(hwnd);
                    finish_open_spawn();
                    return;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let detail = format!(
                            "Live-Status-Prozess wurde beendet, bevor ein Fenster sichtbar wurde ({status})"
                        );
                        if attempt == LIVE_WINDOW_START_ATTEMPTS {
                            record_open_failure(&detail);
                            finish_open_spawn();
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
                            finish_open_spawn();
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
                            finish_open_spawn();
                            return;
                        }
                        record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                        break;
                    }
                }
                thread::sleep(LIVE_WINDOW_POLL_INTERVAL);
            }
            thread::sleep(LIVE_WINDOW_RETRY_DELAY);
            child = match spawn_live_status_process(&executable, owner_pid) {
                Ok(child) => child,
                Err(error) => {
                    record_open_failure(&format!(
                        "Live-Status-Prozess konnte beim Wiederholungsversuch nicht gestartet werden: {error}"
                    ));
                    finish_open_spawn();
                    return;
                }
            };
        }
        finish_open_spawn();
    });
    Ok(())
}

pub fn close() {
    let mut pids = Vec::new();
    if let Some(hwnd) = find_existing_live_window() {
        if let Some(pid) = window_pid(hwnd) {
            pids.push(pid);
        }
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
    }
    if let Ok(gate) = OPEN_ATTEMPT.lock()
        && let Some(pid) = gate.as_ref().and_then(|attempt| attempt.child_pid)
    {
        pids.push(pid);
    }
    pids.retain(|pid| *pid != 0 && *pid != current_pid());
    pids.sort_unstable();
    pids.dedup();
    let started_at = Instant::now();
    while started_at.elapsed() < LIVE_WINDOW_CLOSE_TIMEOUT {
        pids.retain(|pid| process_is_running(*pid));
        if pids.is_empty() && find_existing_live_window().is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    for pid in pids {
        terminate_pid(pid);
    }
    if let Some(hwnd) = find_existing_live_window()
        && let Some(pid) = window_pid(hwnd)
    {
        terminate_pid(pid);
    }
}

fn spawn_live_status_process(executable: &Path, owner_pid: Option<u32>) -> Result<Child> {
    let mut command = Command::new(executable);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--live-status");
    if let Some(owner_pid) = owner_pid {
        command.arg(format!("{OWNER_PID_PREFIX}{owner_pid}"));
    }
    command
        .spawn()
        .context("Live-Status-Fenster konnte nicht gestartet werden")
}

fn find_primary_tray_pid() -> Option<u32> {
    let mut search = TrayWindowQuery { pid: None };
    unsafe {
        let _ = EnumWindows(
            Some(find_primary_tray_window_callback),
            &mut search as *mut _ as LPARAM,
        );
    }
    search.pid
}

fn find_primary_tray_pid_with_retry() -> Option<u32> {
    retry_primary_tray_pid(
        find_primary_tray_pid,
        PRIMARY_TRAY_DISCOVERY_TIMEOUT,
        PRIMARY_TRAY_DISCOVERY_INTERVAL,
    )
}

fn retry_primary_tray_pid<F>(mut find_pid: F, timeout: Duration, interval: Duration) -> Option<u32>
where
    F: FnMut() -> Option<u32>,
{
    let started_at = Instant::now();
    loop {
        if let Some(pid) = find_pid() {
            return Some(pid);
        }
        if started_at.elapsed() >= timeout {
            return None;
        }
        thread::sleep(interval);
    }
}

struct TrayWindowQuery {
    pid: Option<u32>,
}

unsafe extern "system" fn find_primary_tray_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam as *mut TrayWindowQuery);
        if window_class(hwnd) != "tray_icon_app" {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 && pid != current_pid() && process_image_is_this_app(pid) {
            search.pid = Some(pid);
            return 0;
        }
    }
    1
}

fn parse_owner_pid<I, S>(arguments: I) -> Option<u32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    arguments.into_iter().find_map(|argument| {
        argument
            .as_ref()
            .strip_prefix(OWNER_PID_PREFIX)
            .and_then(|value| value.parse().ok())
            .filter(|pid| *pid != 0)
    })
}

fn find_live_window() -> Option<HWND> {
    pick_live_window(None, None)
}

fn find_existing_live_window() -> Option<HWND> {
    // The tray process owns a monitor-sized transparent `tray_icon_app`
    // helper. That HWND must never be treated as the Live-Status window.
    pick_live_window(None, Some(current_pid()))
}

fn find_live_window_for_pid(pid: u32) -> Option<HWND> {
    pick_live_window(Some(pid), None)
}

fn pick_live_window(required_pid: Option<u32>, exclude_pid: Option<u32>) -> Option<HWND> {
    let mut search = LiveWindowQuery {
        required_pid,
        exclude_pid,
        pick: LiveWindowPick::default(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_live_window_callback),
            &mut search as *mut _ as LPARAM,
        );
    }
    search.pick.hwnd
}

struct LiveWindowQuery {
    required_pid: Option<u32>,
    exclude_pid: Option<u32>,
    pick: LiveWindowPick,
}

#[derive(Default)]
struct LiveWindowPick {
    hwnd: Option<HWND>,
    score: i32,
    area: i64,
}

unsafe extern "system" fn find_live_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam as *mut LiveWindowQuery);
        if let Some((score, area)) =
            score_live_window(hwnd, search.required_pid, search.exclude_pid)
            && (score > search.pick.score
                || (score == search.pick.score && area > search.pick.area))
        {
            search.pick.hwnd = Some(hwnd);
            search.pick.score = score;
            search.pick.area = area;
        }
    }
    1
}

fn activate_live_window(hwnd: HWND) {
    if !is_activatable_live_hwnd(hwnd) {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        if window_settings::WindowLevel::current() != window_settings::WindowLevel::AlwaysOnBottom {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn is_live_window_title(title: &str) -> bool {
    title == "Herdr-Nachtwächter - Live-Status" || title == "Herdr Night Watch - Live Status"
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        String::from_utf16_lossy(&title[..copied as usize])
    }
}

fn score_live_window(
    hwnd: HWND,
    required_pid: Option<u32>,
    exclude_pid: Option<u32>,
) -> Option<(i32, i64)> {
    let mut window_pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut window_pid);
    }
    if required_pid.is_none() && !process_image_is_this_app(window_pid) {
        return None;
    }
    let (visible, minimized, width, height) = live_window_metrics(hwnd);
    score_live_window_facts(
        &LiveWindowFacts {
            title: window_title(hwnd),
            class: window_class(hwnd),
            visible,
            minimized,
            width,
            height,
            pid: window_pid,
        },
        required_pid,
        exclude_pid,
    )
}

struct LiveWindowFacts {
    title: String,
    class: String,
    visible: bool,
    minimized: bool,
    width: i32,
    height: i32,
    pid: u32,
}

fn score_live_window_facts(
    facts: &LiveWindowFacts,
    required_pid: Option<u32>,
    exclude_pid: Option<u32>,
) -> Option<(i32, i64)> {
    if is_helper_window_class(&facts.class) {
        return None;
    }
    if let Some(required_pid) = required_pid
        && facts.pid != required_pid
    {
        return None;
    }
    if let Some(exclude_pid) = exclude_pid
        && facts.pid == exclude_pid
    {
        return None;
    }
    if !facts.title.is_empty() && !is_live_window_title(&facts.title) {
        return None;
    }
    if is_dummy_event_target_window(facts.visible, facts.minimized, facts.width, facts.height) {
        return None;
    }
    if facts.title.is_empty()
        && !untitled_window_can_be_live(facts.visible, facts.minimized, facts.width, facts.height)
    {
        return None;
    }
    if !facts.title.is_empty()
        && !is_live_window_candidate(facts.visible, facts.minimized, facts.width, facts.height)
    {
        return None;
    }
    let score = if facts.title.is_empty() { 1 } else { 2 };
    Some((score, i64::from(facts.width) * i64::from(facts.height)))
}

fn window_class(hwnd: HWND) -> String {
    unsafe {
        let mut class_name = [0u16; 256];
        let copied = GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&class_name[..copied as usize])
    }
}

fn is_helper_window_class(class: &str) -> bool {
    matches!(
        class,
        "tray_icon_app" | "Winit Thread Event Target" | "MSCTFIME UI" | "IME"
    )
}

fn current_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

fn process_image_is_this_app(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let current = std::env::current_exe().ok();
    let Some(other) = process_image_path(pid) else {
        return false;
    };
    let Some(current) = current else {
        return other
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case("Herdr-Nachtwaechter"));
    };
    current.file_name() == other.file_name()
}

fn process_image_path(pid: u32) -> Option<std::path::PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 512];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        let _ = CloseHandle(handle);
        if ok == 0 || length == 0 {
            return None;
        }
        Some(std::path::PathBuf::from(String::from_utf16_lossy(
            &buffer[..length as usize],
        )))
    }
}

fn live_window_metrics(hwnd: HWND) -> (bool, bool, i32, i32) {
    unsafe {
        let visible = IsWindowVisible(hwnd) != 0;
        let minimized = IsIconic(hwnd) != 0;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let (width, height) = if GetWindowRect(hwnd, &mut rect) != 0 {
            (rect.right - rect.left, rect.bottom - rect.top)
        } else {
            (0, 0)
        };
        (visible, minimized, width, height)
    }
}

fn is_activatable_live_hwnd(hwnd: HWND) -> bool {
    if is_helper_window_class(&window_class(hwnd)) {
        return false;
    }
    let (visible, minimized, width, height) = live_window_metrics(hwnd);
    is_showable_live_window(visible, minimized, width, height)
}

fn begin_open_spawn() -> bool {
    let Ok(mut gate) = OPEN_ATTEMPT.lock() else {
        return true;
    };
    if let Some(existing) = gate.as_ref() {
        if existing.child_pid.is_some_and(process_is_running) {
            return false;
        }
        if existing.started_at.elapsed() < OPEN_SPAWN_DEBOUNCE {
            return false;
        }
    }
    *gate = Some(OpenAttempt {
        started_at: Instant::now(),
        child_pid: None,
    });
    true
}

fn remember_open_child(pid: u32) {
    if let Ok(mut gate) = OPEN_ATTEMPT.lock()
        && let Some(existing) = gate.as_mut()
    {
        existing.child_pid = Some(pid);
    }
}

fn finish_open_spawn() {
    if let Ok(mut gate) = OPEN_ATTEMPT.lock() {
        *gate = None;
    }
}

fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        let _ = CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    (pid != 0).then_some(pid)
}

fn terminate_pid(pid: u32) {
    if pid == 0 || pid == current_pid() {
        return;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return;
        }
        let _ = TerminateProcess(handle, 0);
        let _ = CloseHandle(handle);
    }
}

fn is_dummy_event_target_window(_visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    !minimized
        && width > 0
        && height > 0
        && width < LIVE_WINDOW_MIN_READY_WIDTH
        && height < LIVE_WINDOW_MIN_READY_HEIGHT
}

fn is_live_window_candidate(visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    if minimized {
        return true;
    }
    // The 4x4 event-target window can be visible or hidden. Never treat that
    // dummy as the live window, even after a reboot when the real window is
    // already full size but still hidden and untitled.
    !is_dummy_event_target_window(visible, minimized, width, height)
}

fn untitled_window_can_be_live(visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    let _ = visible;
    if minimized {
        return true;
    }
    !is_dummy_event_target_window(visible, minimized, width, height)
        && width >= LIVE_WINDOW_MIN_READY_WIDTH
        && height >= LIVE_WINDOW_MIN_READY_HEIGHT
}

fn is_showable_live_window(visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    if is_dummy_event_target_window(visible, minimized, width, height) {
        return false;
    }
    if minimized {
        return true;
    }
    // A real HWND can report 0x0 while it is still hidden. Showing it is the
    // opener's job; dummy and helper windows are rejected before this check.
    if width <= 0 || height <= 0 {
        return true;
    }
    width >= LIVE_WINDOW_MIN_READY_WIDTH && height >= LIVE_WINDOW_MIN_READY_HEIGHT
}

#[cfg_attr(not(test), allow(dead_code))]
fn is_ready_live_window(visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    if minimized {
        return true;
    }
    visible && width >= LIVE_WINDOW_MIN_READY_WIDTH && height >= LIVE_WINDOW_MIN_READY_HEIGHT
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
    let scale = window_settings::live_status_scale();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([DESIGN_WIDTH * scale, DESIGN_HEIGHT * scale])
        .with_min_inner_size([
            DESIGN_WIDTH * window_settings::MIN_LIVE_STATUS_SCALE,
            DESIGN_HEIGHT * window_settings::MIN_LIVE_STATUS_SCALE,
        ])
        .with_max_inner_size([
            DESIGN_WIDTH * window_settings::MAX_LIVE_STATUS_SCALE,
            DESIGN_HEIGHT * window_settings::MAX_LIVE_STATUS_SCALE,
        ])
        .with_resizable(false)
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
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        match Language::current() {
            Language::German => "Herdr-Nachtwächter - Live-Status",
            Language::English => "Herdr Night Watch - Live Status",
        },
        options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.set_zoom_factor(scale);
            configure_visuals(&creation_context.egui_ctx);
            Ok(Box::new(LiveStatusApp::new(
                scale,
                parse_owner_pid(std::env::args()),
            )))
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
    resize_drag: Option<(egui::Pos2, f32, egui::Vec2)>,
    resize_preview_scale: Option<f32>,
    resize_preview_window_size: Option<egui::Vec2>,
    context_menu_pos: Option<egui::Pos2>,
    scale: f32,
    last_saved_position: Option<[f32; 2]>,
    owner_pid: Option<u32>,
}

impl LiveStatusApp {
    fn new(scale: f32, owner_pid: Option<u32>) -> Self {
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
            resize_drag: None,
            resize_preview_scale: None,
            resize_preview_window_size: None,
            context_menu_pos: None,
            scale,
            last_saved_position: window_settings::live_status_position(),
            owner_pid,
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
        if let Some(owner_pid) = self.owner_pid
            && !process_is_running(owner_pid)
        {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
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
                                        metric(ui, self.language.text("Erkannt", "Detected"), agents.total, TEXT, false);
                                        divider(ui);
                                        metric(ui, self.language.text("Arbeitet", "Working"), agents.working, GREEN, false);
                                        divider(ui);
                                        metric(ui, self.language.text("Bereit", "Ready"), agents.idle, ACCENT, false);
                                        divider(ui);
                                        let finished = agents.done > 0;
                                        metric(
                                            ui,
                                            self.language.text("Fertig", "Finished"),
                                            agents.done,
                                            if finished { GREEN } else { GRAY },
                                            finished,
                                        );
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
        handle_context_menu(self, ui);
        self.show_toast(ui.ctx());
        handle_window_resize(self, ui);
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

fn apply_live_status_scale(
    app: &mut LiveStatusApp,
    ctx: &egui::Context,
    requested_scale: f32,
    persist: bool,
) {
    let scale = window_settings::clamp_live_status_scale(requested_scale);
    let previous_zoom = ctx.zoom_factor().max(f32::EPSILON);
    app.scale = scale;
    ctx.set_zoom_factor(scale);
    // ViewportCommand sizes are interpreted with the zoom factor that is
    // active for the current pass. Compensate for that so the physical window
    // reaches the requested size immediately, without a one-frame jump.
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
        DESIGN_WIDTH * scale / previous_zoom,
        DESIGN_HEIGHT * scale / previous_zoom,
    )));
    if persist {
        let _ = window_settings::set_live_status_scale(scale);
    }
}

fn physical_pointer(ctx: &egui::Context, position: egui::Pos2) -> egui::Pos2 {
    let pixels_per_point = ctx.pixels_per_point();
    egui::pos2(position.x * pixels_per_point, position.y * pixels_per_point)
}

fn resize_grip_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(
            rect.right() - RESIZE_GRIP_SIZE,
            rect.bottom() - RESIZE_GRIP_SIZE,
        ),
        rect.right_bottom(),
    )
}

fn handle_window_resize(app: &mut LiveStatusApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let grip = resize_grip_rect(rect);
    let response = ui.interact(
        grip,
        ui.make_persistent_id("live_window_resize"),
        egui::Sense::drag(),
    );
    if response.drag_started() {
        let pointer = ui
            .ctx()
            .input(|input| input.pointer.interact_pos())
            .unwrap_or(grip.right_bottom());
        let initial_window_size = ui
            .ctx()
            .input(|input| input.viewport().inner_rect.map(|rect| rect.size()))
            .unwrap_or_else(|| rect.size());
        app.resize_drag = Some((
            physical_pointer(ui.ctx(), pointer),
            app.scale,
            initial_window_size,
        ));
        app.resize_preview_scale = Some(app.scale);
        app.resize_preview_window_size = None;
    }
    if let Some((start_pointer, initial_scale, initial_window_size)) = app.resize_drag {
        let primary_down = ui.ctx().input(|input| input.pointer.primary_down());
        if primary_down && let Some(pointer) = ui.ctx().input(|input| input.pointer.interact_pos())
        {
            let pointer = physical_pointer(ui.ctx(), pointer);
            let delta = pointer.x - start_pointer.x;
            let native_pixels_per_point = ui.ctx().native_pixels_per_point().unwrap_or(1.0);
            let scale_delta = delta / (DESIGN_WIDTH * native_pixels_per_point);
            let requested_scale = initial_scale + scale_delta;
            let preview_scale = window_settings::clamp_live_status_scale(requested_scale);
            app.resize_preview_scale = Some(preview_scale);

            // Grow the native surface for a larger target. Never shrink it
            // during the drag: the frozen layout would cover the percent and
            // pixel label. The smaller target is drawn inside instead.
            let preview_window_size =
                initial_window_size * (preview_scale / initial_scale.max(f32::EPSILON));
            let native_preview_size =
                resize_drag_native_size(initial_window_size, preview_window_size);
            let should_resize = app.resize_preview_window_size.is_none_or(|last| {
                (last.x - native_preview_size.x).abs() > 1.0
                    || (last.y - native_preview_size.y).abs() > 1.0
            });
            if should_resize {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::InnerSize(native_preview_size));
                app.resize_preview_window_size = Some(native_preview_size);
            }
        }

        // Keep the actual layout frozen while the pointer is held. Applying a
        // new zoom here would move the very grip that is currently being
        // dragged, which can make a release look like a continued drag.
        if response.drag_stopped() || !primary_down {
            if let Some(preview_scale) = app.resize_preview_scale {
                apply_live_status_scale(app, ui.ctx(), preview_scale, true);
            }
            app.resize_drag = None;
            app.resize_preview_scale = None;
            app.resize_preview_window_size = None;
            // Prevent the release frame from falling through into
            // StartDrag when the platform reports the button state one frame
            // late.
            app.window_drag_started = true;
        }
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    let color = if response.hovered() || response.dragged() {
        egui::Color32::from_rgba_unmultiplied(226, 232, 240, 150)
    } else {
        egui::Color32::from_rgba_unmultiplied(148, 163, 184, 65)
    };
    for offset in [0.0, 5.0, 10.0] {
        ui.painter().line_segment(
            [
                egui::pos2(grip.right() - 4.0 - offset, grip.bottom() - 1.0),
                egui::pos2(grip.right() - 1.0, grip.bottom() - 4.0 - offset),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
    if let Some(preview_scale) = app.resize_preview_scale {
        // The native viewport stays at least as large as the drag start.
        // A smaller target is drawn inside that surface so the frozen
        // layout cannot cover the percent and pixel label.
        let preview_rect = resize_preview_target_rect(rect, app.scale, preview_scale);
        let preview_color = egui::Color32::from_rgba_unmultiplied(147, 197, 253, 230);
        // The target is a clearly visible dark-blue surface at roughly 50 %
        // opacity, not just a thin outline. The old content remains readable
        // underneath so the user can still orient themselves while dragging.
        let preview_fill = egui::Color32::from_rgba_unmultiplied(15, 31, 56, 128);
        ui.painter()
            .rect_filled(preview_rect, egui::CornerRadius::same(8), preview_fill);
        ui.painter().rect_stroke(
            preview_rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0, preview_color),
            egui::StrokeKind::Inside,
        );
        ui.painter().rect_stroke(
            preview_rect.shrink(4.0),
            egui::CornerRadius::same(6),
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(226, 232, 240, 65),
            ),
            egui::StrokeKind::Inside,
        );
        let target_width = (DESIGN_WIDTH * preview_scale).round() as u32;
        let target_height = (DESIGN_HEIGHT * preview_scale).round() as u32;
        let label = format!(
            "{:.0} % · {} × {} px",
            preview_scale * 100.0,
            target_width,
            target_height
        );
        let galley = ui
            .painter()
            .layout_no_wrap(label, egui::FontId::proportional(10.0), TEXT);
        let label_rect = resize_preview_label_rect(rect, preview_rect, galley.size());
        ui.painter().rect_filled(
            label_rect,
            egui::CornerRadius::same(5),
            egui::Color32::from_rgba_unmultiplied(15, 23, 42, 225),
        );
        ui.painter().rect_stroke(
            label_rect,
            egui::CornerRadius::same(5),
            egui::Stroke::new(1.0, preview_color),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .galley(label_rect.center() - galley.size() / 2.0, galley, TEXT);
    }
}

fn resize_drag_native_size(
    initial_window_size: egui::Vec2,
    preview_window_size: egui::Vec2,
) -> egui::Vec2 {
    if preview_window_size.x + 0.5 < initial_window_size.x
        || preview_window_size.y + 0.5 < initial_window_size.y
    {
        initial_window_size
    } else {
        preview_window_size
    }
}

fn resize_preview_target_rect(
    window_rect: egui::Rect,
    current_scale: f32,
    preview_scale: f32,
) -> egui::Rect {
    let current_scale = current_scale.max(f32::EPSILON);
    if preview_scale + f32::EPSILON >= current_scale {
        return window_rect.shrink(4.0);
    }
    let ratio = (preview_scale / current_scale).clamp(0.0, 1.0);
    egui::Rect::from_min_size(window_rect.min, window_rect.size() * ratio).shrink(4.0)
}

fn resize_preview_label_rect(
    window_rect: egui::Rect,
    preview_rect: egui::Rect,
    label_size: egui::Vec2,
) -> egui::Rect {
    let size = label_size + egui::vec2(12.0, 5.0);
    let preferred = egui::Rect::from_center_size(
        egui::pos2(preview_rect.center().x, preview_rect.bottom() - 10.0),
        size,
    );
    clamp_rect_inside(preferred, window_rect.shrink(4.0))
}

fn clamp_rect_inside(rect: egui::Rect, bounds: egui::Rect) -> egui::Rect {
    if bounds.width() < rect.width() || bounds.height() < rect.height() {
        return egui::Rect::from_center_size(bounds.center(), rect.size().min(bounds.size()));
    }
    let mut min = rect.min;
    if rect.left() < bounds.left() {
        min.x = bounds.left();
    }
    if rect.right() > bounds.right() {
        min.x = bounds.right() - rect.width();
    }
    if rect.top() < bounds.top() {
        min.y = bounds.top();
    }
    if rect.bottom() > bounds.bottom() {
        min.y = bounds.bottom() - rect.height();
    }
    egui::Rect::from_min_size(min, rect.size())
}

fn is_window_interactive_position(rect: egui::Rect, position: egui::Pos2) -> bool {
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
    completion_switch.contains(position)
        || warning_seconds.contains(position)
        || moon.contains(position)
        || control_hood.contains(position)
        || weather_control_rect(rect).contains(position)
        || resize_grip_rect(rect).contains(position)
}

fn handle_context_menu(app: &mut LiveStatusApp, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let (pointer, secondary_clicked, any_click) = ui.ctx().input(|input| {
        (
            input.pointer.interact_pos(),
            input.pointer.secondary_clicked(),
            input.pointer.any_click(),
        )
    });
    if secondary_clicked
        && pointer.is_some_and(|position| !is_window_interactive_position(rect, position))
    {
        app.context_menu_pos = pointer;
    }
    let Some(menu_position) = app.context_menu_pos else {
        return;
    };

    let scale_label = format!("{:.0} %", app.scale * 100.0);
    let mut reset_requested = false;
    let menu = egui::Area::new(egui::Id::new("live_status_context_menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(menu_position + egui::vec2(4.0, 4.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}: {scale_label}",
                        app.language.text("Fenstergröße", "Window size")
                    ))
                    .color(GRAY),
                );
                if ui
                    .button(app.language.text("Auf 100 % zurücksetzen", "Reset to 100%"))
                    .clicked()
                {
                    reset_requested = true;
                }
            });
        });
    if reset_requested {
        apply_live_status_scale(
            app,
            ui.ctx(),
            window_settings::DEFAULT_LIVE_STATUS_SCALE,
            false,
        );
        let _ = window_settings::reset_live_status_scale();
        app.context_menu_pos = None;
        app.toast = Some(Toast {
            message: app
                .language
                .text("Fenster auf 100 % zurückgesetzt", "Window reset to 100%")
                .into(),
            color: ACCENT,
            expires_at: Instant::now() + Duration::from_secs(3),
        });
    } else if any_click
        && !secondary_clicked
        && pointer.is_some_and(|position| !menu.response.rect.contains(position))
    {
        app.context_menu_pos = None;
    }
}

fn handle_window_drag(app: &mut LiveStatusApp, ui: &egui::Ui) {
    // A resize drag must never fall through to the window-drag handler. Both
    // gestures use the primary button; starting a native window drag here
    // makes the whole window move instead of keeping it fixed for the preview.
    if app.resize_drag.is_some() || app.resize_preview_scale.is_some() {
        return;
    }
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
    let is_excluded = |position: egui::Pos2| {
        completion_switch.contains(position)
            || warning_seconds.contains(position)
            || moon.contains(position)
            || control_hood.contains(position)
            || weather_control_rect(rect).contains(position)
            || resize_grip_rect(rect).contains(position)
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
            radius,
            dark_side,
            separation,
            illumination,
            gradient_color_at(gradient_rect, center),
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

fn moon_temperature_label(temperature_c: f64) -> String {
    format!("{temperature_c:.0}°C")
}

fn outline_width_in_points(pixels_per_point: f32) -> f32 {
    1.0 / pixels_per_point.max(f32::EPSILON)
}

fn outline_offsets(width: f32) -> [(f32, f32); 16] {
    let mut offsets = [(0.0, 0.0); 16];
    for (step, offset) in offsets.iter_mut().enumerate() {
        let angle = std::f32::consts::TAU * step as f32 / 16.0;
        *offset = (width * angle.cos(), width * angle.sin());
    }
    offsets
}

fn lit_sickle_clip_rect(
    moon_rect: egui::Rect,
    center: egui::Pos2,
    radius: f32,
    dark_side: f32,
    separation: f32,
    illumination: f32,
) -> Option<egui::Rect> {
    if illumination <= 0.12 {
        return None;
    }
    if illumination >= 0.88 {
        return Some(moon_rect);
    }
    let split_x = center.x + dark_side * (separation - radius);
    if dark_side > 0.0 {
        Some(egui::Rect::from_min_max(
            moon_rect.min,
            egui::pos2(split_x, moon_rect.bottom()),
        ))
    } else {
        Some(egui::Rect::from_min_max(
            egui::pos2(split_x, moon_rect.top()),
            moon_rect.max,
        ))
    }
}

fn paint_outlined_label(
    painter: &egui::Painter,
    center: egui::Pos2,
    text: &str,
    font: egui::FontId,
    fill: egui::Color32,
    outline: egui::Color32,
) {
    let width = outline_width_in_points(painter.pixels_per_point());
    for (dx, dy) in outline_offsets(width) {
        painter.text(
            center + egui::vec2(dx, dy),
            egui::Align2::CENTER_CENTER,
            text,
            font.clone(),
            outline,
        );
    }
    painter.text(center, egui::Align2::CENTER_CENTER, text, font, fill);
}

fn paint_moon_temperature(
    painter: &egui::Painter,
    moon_rect: egui::Rect,
    center: egui::Pos2,
    radius: f32,
    dark_side: f32,
    separation: f32,
    illumination: f32,
    background: egui::Color32,
    temperature_c: f64,
) {
    let text = moon_temperature_label(temperature_c);
    let font = egui::FontId::proportional(15.0);
    let fill = egui::Color32::from_rgb(247, 241, 229);
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        &text,
        font.clone(),
        fill,
    );
    if let Some(lit_rect) = lit_sickle_clip_rect(
        moon_rect,
        center,
        radius,
        dark_side,
        separation,
        illumination,
    ) {
        paint_outlined_label(
            &painter.with_clip_rect(lit_rect),
            center,
            &text,
            font,
            fill,
            background,
        );
    }
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

fn metric(ui: &mut egui::Ui, label: &str, value: usize, color: egui::Color32, highlight: bool) {
    ui.vertical(|ui| {
        let value_text = value.to_string();
        if highlight {
            glowing_metric_text(ui, &value_text, 26.0, color);
        } else {
            ui.label(
                egui::RichText::new(value_text)
                    .size(26.0)
                    .strong()
                    .color(color),
            );
        }
        ui.label(
            egui::RichText::new(label)
                .small()
                .color(if highlight { GREEN } else { GRAY }),
        );
    });
}

fn paint_text_halo(
    painter: &egui::Painter,
    origin: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) {
    for (dx, dy) in outline_offsets(width) {
        painter.text(
            origin + egui::vec2(dx, dy),
            egui::Align2::LEFT_TOP,
            text,
            font.clone(),
            color,
        );
    }
}

fn glowing_metric_text(ui: &mut egui::Ui, text: &str, size: f32, color: egui::Color32) {
    let font = egui::FontId::proportional(size);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), color);
    let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    let painter = ui.painter();
    let pixels_per_point = painter.pixels_per_point();
    paint_text_halo(
        painter,
        rect.min,
        text,
        font.clone(),
        egui::Color32::from_rgba_unmultiplied(255, 215, 110, 38),
        3.0 / pixels_per_point.max(f32::EPSILON),
    );
    paint_text_halo(
        painter,
        rect.min,
        text,
        font.clone(),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 88),
        outline_width_in_points(pixels_per_point),
    );
    painter.text(rect.min, egui::Align2::LEFT_TOP, text, font, color);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_halo_rim_stays_one_screen_pixel() {
        assert!((outline_width_in_points(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((outline_width_in_points(3.0) - (1.0 / 3.0)).abs() < 0.001);
    }

    #[test]
    fn moon_temperature_stays_whole_and_outlined() {
        assert_eq!(moon_temperature_label(17.4), "17°C");
        assert!((outline_width_in_points(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((outline_width_in_points(2.0) - 0.5).abs() < f32::EPSILON);
        let offsets = outline_offsets(1.0);
        assert_eq!(offsets.len(), 16);
        for (dx, dy) in offsets {
            let length = (dx * dx + dy * dy).sqrt();
            assert!((length - 1.0).abs() < 0.001);
        }
    }

    #[test]
    fn temperature_outline_covers_only_the_lit_sickle() {
        let moon = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let center = moon.center();
        assert_eq!(
            lit_sickle_clip_rect(moon, center, 50.0, 1.0, 50.0, 0.05),
            None
        );
        assert_eq!(
            lit_sickle_clip_rect(moon, center, 50.0, 1.0, 50.0, 0.95),
            Some(moon)
        );
        let half = lit_sickle_clip_rect(moon, center, 50.0, 1.0, 50.0, 0.5).unwrap();
        assert!(half.right() <= center.x + 0.5);
        assert!(half.left() <= moon.left());
    }

    #[test]
    fn live_window_titles_are_recognized() {
        assert!(is_live_window_title("Herdr-Nachtwächter - Live-Status"));
        assert!(is_live_window_title("Herdr Night Watch - Live Status"));
        assert!(!is_live_window_title(""));
        assert!(!is_live_window_title(
            "Herdr-Nachtwächter - Abschlussprotokoll"
        ));
    }

    #[test]
    fn owner_pid_is_read_from_spawn_argument() {
        assert_eq!(
            parse_owner_pid(["--live-status", "--owner-pid=3648"]),
            Some(3648)
        );
        assert_eq!(parse_owner_pid(["--live-status"]), None);
        assert_eq!(parse_owner_pid(["--owner-pid=0"]), None);
        assert_eq!(parse_owner_pid(["--owner-pid=abc"]), None);
    }

    #[test]
    fn secondary_open_retries_until_primary_tray_is_registered() {
        let mut attempts = 0;
        let pid = retry_primary_tray_pid(
            || {
                attempts += 1;
                (attempts >= 3).then_some(3648)
            },
            Duration::from_secs(1),
            Duration::from_millis(1),
        );
        assert_eq!(pid, Some(3648));
        assert_eq!(attempts, 3);
    }

    #[test]
    fn dummy_event_target_window_is_not_the_live_window() {
        assert!(is_dummy_event_target_window(true, false, 4, 4));
        assert!(is_dummy_event_target_window(false, false, 4, 4));
        assert!(!is_live_window_candidate(true, false, 4, 4));
        assert!(!is_live_window_candidate(false, false, 4, 4));
        assert!(!is_ready_live_window(true, false, 4, 4));
        assert!(!is_showable_live_window(true, false, 4, 4));
    }

    #[test]
    fn tray_helper_window_is_never_the_live_window() {
        let tray_helper = LiveWindowFacts {
            title: String::new(),
            class: "tray_icon_app".into(),
            visible: true,
            minimized: false,
            width: 1920,
            height: 1025,
            pid: 3648,
        };
        assert!(is_helper_window_class(&tray_helper.class));
        assert!(score_live_window_facts(&tray_helper, None, Some(3648)).is_none());
        assert!(score_live_window_facts(&tray_helper, None, None).is_none());
    }

    #[test]
    fn winit_and_ime_helpers_are_never_the_live_window() {
        for class in ["Winit Thread Event Target", "MSCTFIME UI", "IME"] {
            let helper = LiveWindowFacts {
                title: String::new(),
                class: class.into(),
                visible: true,
                minimized: false,
                width: 4,
                height: 4,
                pid: 10,
            };
            assert!(score_live_window_facts(&helper, Some(10), None).is_none());
        }
    }

    #[test]
    fn titled_live_window_beats_same_process_helpers() {
        let live = LiveWindowFacts {
            title: "Herdr-Nachtwächter - Live-Status".into(),
            class: "Window Class".into(),
            visible: false,
            minimized: false,
            width: 393,
            height: 190,
            pid: 99,
        };
        assert_eq!(
            score_live_window_facts(&live, None, Some(3648)),
            Some((2, 393 * 190))
        );
        assert!(score_live_window_facts(&live, None, Some(99)).is_none());
    }

    #[test]
    fn hidden_zero_size_titled_window_can_be_shown() {
        assert!(is_live_window_candidate(false, false, 0, 0));
        assert!(is_showable_live_window(false, false, 0, 0));
        assert!(!untitled_window_can_be_live(false, false, 0, 0));
    }

    #[test]
    fn hidden_real_window_is_a_candidate_and_can_be_shown() {
        assert!(is_live_window_candidate(false, false, 393, 190));
        assert!(!is_ready_live_window(false, false, 393, 190));
        assert!(is_showable_live_window(false, false, 393, 190));
        assert!(untitled_window_can_be_live(false, false, 1920, 1025));
        assert!(!untitled_window_can_be_live(true, false, 4, 4));
    }

    #[test]
    fn painted_or_minimized_live_window_is_ready() {
        assert!(is_ready_live_window(true, false, 393, 190));
        assert!(is_ready_live_window(false, true, 4, 4));
        assert!(is_live_window_candidate(false, true, 4, 4));
    }

    #[test]
    fn shrink_drag_keeps_the_native_window_at_the_start_size() {
        let initial = egui::vec2(1179.0, 570.0);
        let smaller = egui::vec2(393.0, 190.0);
        assert_eq!(resize_drag_native_size(initial, smaller), initial);
        assert_eq!(resize_drag_native_size(initial, initial), initial);
        let larger = egui::vec2(1572.0, 760.0);
        assert_eq!(resize_drag_native_size(initial, larger), larger);
    }

    #[test]
    fn shrink_preview_stays_inside_the_current_window() {
        let window = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(900.0, 450.0));
        let preview = resize_preview_target_rect(window, 3.0, 1.0);
        assert!(window.contains_rect(preview));
        assert!((preview.width() - (900.0 / 3.0 - 8.0)).abs() < 0.01);
        assert!((preview.height() - (450.0 / 3.0 - 8.0)).abs() < 0.01);
        let grown = resize_preview_target_rect(window, 1.0, 3.0);
        assert_eq!(grown, window.shrink(4.0));
    }

    #[test]
    fn resize_preview_label_stays_inside_the_window() {
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(393.0, 190.0));
        let preview = resize_preview_target_rect(window, 3.0, 1.0);
        let label = resize_preview_label_rect(window, preview, egui::vec2(120.0, 14.0));
        assert!(window.shrink(4.0).contains_rect(label));
        assert!(label.width() > 120.0);
        assert!(label.height() > 14.0);
    }
}
