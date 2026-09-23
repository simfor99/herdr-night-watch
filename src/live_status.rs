use crate::{
    autostart,
    backend::{self, AgentSummary, CompletionAction, WatchStatus},
    language::Language,
    log_viewer,
    media::{self, MediaCommand, MediaSnapshot},
    quota::{PacingForecast, PacingHealth, ProviderId, ProviderQuota, QuotaSnapshot},
    settings,
    system_metrics::{self, SystemMetrics},
    taskbar,
    weather::{self, WeatherLocation, WeatherReading, WeatherSymbol},
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
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{
    BOOL, CloseHandle, GetLastError, HANDLE, HWND, LPARAM, RECT, SYSTEMTIME,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
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
const LIVE_WINDOW_START_ATTEMPTS: usize = 3;
const LIVE_WINDOW_RETRY_DELAY: Duration = Duration::from_secs(2);
// eframe keeps the native viewport hidden until it has presented its first
// frame. Until then an existing HWND does not count as drawn, so the watchdog
// waits for visibility instead of bare window existence.
const LIVE_WINDOW_VISIBLE_TIMEOUT: Duration = Duration::from_secs(30);
// After the window became visible a cold-boot GPU failure can still kill the
// process a few frames later. Guard that phase too; a clean exit code 0 there
// is the user closing the window on purpose and must not trigger a reopen.
const LIVE_WINDOW_POST_OPEN_GUARD: Duration = Duration::from_secs(60);
// A retry after an unpainted hang or a post-open death waits longer than the
// regular spawn retry: the graphics driver needs time to finish waking up.
const LIVE_WINDOW_GUARD_RETRY_DELAY: Duration = Duration::from_secs(10);
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
const WINDOW_CONTROL_HOOD_FILL: egui::Color32 = egui::Color32::from_rgb(18, 27, 45);
const WINDOW_CONTROL_HOOD_STROKE: egui::Color32 = egui::Color32::from_rgb(66, 74, 98);
const WINDOW_DRAG_THRESHOLD_SQUARED: f32 = 4.0;
const DESIGN_WIDTH: f32 = 393.0;
const DESIGN_HEIGHT: f32 = 190.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderColorDef {
    pub name_de: &'static str,
    pub name_en: &'static str,
    pub color: egui::Color32,
}

pub const PROVIDER_PASTEL_PALETTE: [ProviderColorDef; 10] = [
    ProviderColorDef {
        name_de: "Flieder",
        name_en: "Lavender",
        color: egui::Color32::from_rgb(213, 162, 250), // Hue 275 (AGI default)
    },
    ProviderColorDef {
        name_de: "Himmelblau",
        name_en: "Sky Blue",
        color: egui::Color32::from_rgb(162, 206, 250), // Hue 210
    },
    ProviderColorDef {
        name_de: "Aquamarin",
        name_en: "Teal",
        color: egui::Color32::from_rgb(162, 250, 250), // Hue 180
    },
    ProviderColorDef {
        name_de: "Whisper Jade",
        name_en: "Mint",
        color: egui::Color32::from_rgb(162, 250, 199), // Hue 145 (Codex default)
    },
    ProviderColorDef {
        name_de: "Pistazie",
        name_en: "Lime",
        color: egui::Color32::from_rgb(213, 250, 162), // Hue 85
    },
    ProviderColorDef {
        name_de: "Goldgelb",
        name_en: "Butter Gold",
        color: egui::Color32::from_rgb(250, 232, 162), // Hue 48
    },
    ProviderColorDef {
        name_de: "Aprikose",
        name_en: "Warm Amber",
        color: egui::Color32::from_rgb(250, 203, 162), // Hue 28 (GLM / Claude default)
    },
    ProviderColorDef {
        name_de: "Pastellkorall",
        name_en: "Coral",
        color: egui::Color32::from_rgb(250, 171, 162), // Hue 6
    },
    ProviderColorDef {
        name_de: "Flamingo",
        name_en: "Rose",
        color: egui::Color32::from_rgb(250, 162, 206), // Hue 330
    },
    ProviderColorDef {
        name_de: "Blaulila",
        name_en: "Soft Indigo",
        color: egui::Color32::from_rgb(170, 162, 250), // Hue 245
    },
];

pub fn provider_palette_color(index: u8) -> egui::Color32 {
    let idx = (index as usize) % PROVIDER_PASTEL_PALETTE.len();
    PROVIDER_PASTEL_PALETTE[idx].color
}

// Five screen pixels at the standard 150 % Windows scaling.
const KPI_ROW_TOP_GAP: f32 = 9.3;
const RESIZE_GRIP_SIZE: f32 = 16.0;
const LIVE_WINDOW_MIN_READY_WIDTH: i32 = 80;
const LIVE_WINDOW_MIN_READY_HEIGHT: i32 = 40;
const STILL_ACTIVE: u32 = 259;
const OPEN_SPAWN_DEBOUNCE: Duration = Duration::from_millis(750);
const LIVE_WINDOW_CLOSE_TIMEOUT: Duration = Duration::from_millis(1500);
const LIVE_INSTANCE_RELEASE_DELAY: Duration = Duration::from_millis(300);
const OWNER_PID_PREFIX: &str = "--owner-pid=";
const LIVE_OPEN_ATTEMPT_MUTEX: &str = "Local\\HerdrNachtwaechter.LiveOpenAttempt";
const LIVE_STATUS_OPEN_FAILURE_EVENT: &str = "live_status_open_failed";
const LIVE_STATUS_PANIC_EVENT: &str = "live_status_panic";
#[cfg(test)]
const LIVE_OPEN_ATTEMPT_MUTEX_ENV: &str = "HERDR_LIVE_OPEN_ATTEMPT_MUTEX";

struct OpenAttempt {
    id: u64,
    started_at: Instant,
    child_pid: Option<u32>,
    process_gate: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExistingWindowDisposition {
    Activate,
    WaitForActiveAttempt,
    Replace(Option<u32>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DuplicateInstanceDisposition {
    Activate(HWND),
    Retry,
}

static OPEN_ATTEMPT: Mutex<Option<OpenAttempt>> = Mutex::new(None);
static NEXT_OPEN_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
// The pid of the most recently spawned live child, kept beyond the open
// attempt so later opens and a tray quit can still clean it up.
static LAST_LIVE_CHILD_PID: Mutex<Option<u32>> = Mutex::new(None);
const MOON_SLOT_WIDTH: f32 = 83.0;
const MOON_ICON_DIAMETER: f32 = 59.5;
const MOON_RIGHT_INSET: f32 = 13.5;
const MOON_HALO_INNER_RADIUS: f32 = 1.16;
const MOON_HALO_OUTER_RADIUS: f32 = 1.35;
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

fn existing_window_disposition(
    ready: bool,
    window_pid: Option<u32>,
    active_child_pid: Option<u32>,
) -> ExistingWindowDisposition {
    if ready {
        ExistingWindowDisposition::Activate
    } else if window_pid.is_some() && window_pid == active_child_pid {
        ExistingWindowDisposition::WaitForActiveAttempt
    } else {
        ExistingWindowDisposition::Replace(window_pid)
    }
}

fn duplicate_instance_disposition(
    hwnd: Option<HWND>,
    is_ready: impl FnOnce(HWND) -> bool,
) -> DuplicateInstanceDisposition {
    match hwnd {
        Some(hwnd) if is_ready(hwnd) => DuplicateInstanceDisposition::Activate(hwnd),
        _ => DuplicateInstanceDisposition::Retry,
    }
}

fn open_with_owner(owner_pid: Option<u32>) -> Result<()> {
    let mut stale_window_pid = None;
    if let Some(hwnd) = find_existing_live_window() {
        apply_taskbar_visibility(hwnd);
        match existing_window_disposition(
            live_window_is_ready_now(hwnd),
            window_pid(hwnd),
            active_open_child_pid(),
        ) {
            ExistingWindowDisposition::Activate => {
                activate_live_window(hwnd);
                return Ok(());
            }
            ExistingWindowDisposition::WaitForActiveAttempt => return Ok(()),
            ExistingWindowDisposition::Replace(pid) => stale_window_pid = pid,
        }
    }

    let Some(open_attempt_id) = begin_open_spawn() else {
        return Ok(());
    };

    if let Some(stale_pid) = stale_window_pid.or_else(stale_live_child_pid) {
        terminate_pid(stale_pid);
        // Give Windows a moment to release the instance mutex of the
        // terminated child before the replacement claims it.
        thread::sleep(LIVE_INSTANCE_RELEASE_DELAY);
    }

    let executable =
        std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
    let mut child = match spawn_live_status_process(&executable, owner_pid) {
        Ok(child) => child,
        Err(error) => {
            finish_open_spawn(open_attempt_id);
            return Err(error);
        }
    };
    if !remember_open_child(open_attempt_id, child.id()) {
        terminate_and_reap_child(&mut child);
        return Ok(());
    }

    thread::spawn(move || {
        let mut child = child;
        for attempt in 1..=LIVE_WINDOW_START_ATTEMPTS {
            if !remember_open_child(open_attempt_id, child.id()) {
                terminate_and_reap_child(&mut child);
                return;
            }
            let mut retry_delay = LIVE_WINDOW_RETRY_DELAY;
            match await_live_window_ready(&mut child) {
                LiveWatchOutcome::Ready(hwnd) => {
                    // Reveal ran inside the child after its first painted
                    // frame; activating an unpainted HWND here would
                    // reintroduce the white flash window.
                    activate_live_window(hwnd);
                    match guard_live_window_after_ready(&mut child) {
                        GuardOutcome::Done => {
                            finish_open_spawn(open_attempt_id);
                            return;
                        }
                        GuardOutcome::Retry(detail) => {
                            if !live_window_attempt_can_retry(attempt) {
                                record_open_failure(&detail);
                                finish_open_spawn(open_attempt_id);
                                return;
                            }
                            record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                            retry_delay = LIVE_WINDOW_GUARD_RETRY_DELAY;
                        }
                    }
                }
                LiveWatchOutcome::ChildExited(detail) => {
                    if !live_window_attempt_can_retry(attempt) {
                        record_open_failure(&detail);
                        finish_open_spawn(open_attempt_id);
                        return;
                    }
                    record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                }
                LiveWatchOutcome::ChildExitedCleanly => {
                    finish_open_spawn(open_attempt_id);
                    return;
                }
                LiveWatchOutcome::NoWindowTimeout => {
                    let detail = no_window_timeout_detail();
                    if !live_window_attempt_can_retry(attempt) {
                        record_open_failure(&detail);
                        finish_open_spawn(open_attempt_id);
                        return;
                    }
                    record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                }
                LiveWatchOutcome::UnpaintedTimeout => {
                    let detail = unpainted_timeout_detail();
                    if !live_window_attempt_can_retry(attempt) {
                        record_open_failure(&detail);
                        finish_open_spawn(open_attempt_id);
                        return;
                    }
                    record_open_failure(&format!("{detail}; neuer Versuch wird gestartet"));
                    retry_delay = LIVE_WINDOW_GUARD_RETRY_DELAY;
                }
            }
            thread::sleep(retry_delay);
            child = match spawn_live_status_process(&executable, owner_pid) {
                Ok(mut child) => {
                    // Register the replacement immediately so a concurrent
                    // close() cannot miss it before the loop restarts.
                    if !remember_open_child(open_attempt_id, child.id()) {
                        terminate_and_reap_child(&mut child);
                        return;
                    }
                    child
                }
                Err(error) => {
                    record_open_failure(&format!(
                        "Live-Status-Prozess konnte beim Wiederholungsversuch nicht gestartet werden: {error}"
                    ));
                    finish_open_spawn(open_attempt_id);
                    return;
                }
            };
        }
        finish_open_spawn(open_attempt_id);
    });
    Ok(())
}

fn no_window_timeout_detail() -> String {
    format!(
        "Live-Status-Prozess läuft, aber nach {} Sekunden wurde kein Fenster gefunden",
        LIVE_WINDOW_START_TIMEOUT.as_secs()
    )
}

fn unpainted_timeout_detail() -> String {
    format!(
        "Live-Status-Fenster wurde gefunden, blieb aber nach {} Sekunden ungezeichnet",
        LIVE_WINDOW_VISIBLE_TIMEOUT.as_secs()
    )
}

enum LiveWatchOutcome {
    /// The window exists and reported itself drawn (visible or minimized).
    Ready(HWND),
    /// The process exited before any window became visible; carries the
    /// already formatted log detail.
    ChildExited(String),
    /// The process stopped cleanly before presentation, for example because
    /// the user closed it immediately. This must not trigger a retry.
    ChildExitedCleanly,
    /// The process lives but never created a window in time.
    NoWindowTimeout,
    /// A window exists but never became visible in time.
    UnpaintedTimeout,
}

fn live_window_attempt_can_retry(attempt: usize) -> bool {
    attempt < LIVE_WINDOW_START_ATTEMPTS
}

/// Phase one and two of the watchdog: wait until the child presents a live
/// window (not merely creates its HWND), the child exits, or a timeout hits.
/// A cold boot can leave the graphics stack unable to paint for a while, so
/// existence alone must not count as success.
fn await_live_window_ready(child: &mut Child) -> LiveWatchOutcome {
    let started_at = Instant::now();
    let mut found_at: Option<Instant> = None;
    let mut hwnd: Option<HWND> = None;
    loop {
        if hwnd.is_none()
            && let Some(found) = find_live_window_for_pid(child.id())
        {
            apply_taskbar_visibility(found);
            hwnd = Some(found);
            found_at = Some(Instant::now());
        }
        if let Some(hwnd) = hwnd
            && live_window_is_ready_now(hwnd)
        {
            return LiveWatchOutcome::Ready(hwnd);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return LiveWatchOutcome::ChildExitedCleanly;
            }
            Ok(Some(status)) => {
                return LiveWatchOutcome::ChildExited(format!(
                    "Live-Status-Prozess wurde beendet, bevor ein Fenster sichtbar wurde ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                terminate_and_reap_child(child);
                return LiveWatchOutcome::ChildExited(format!(
                    "Live-Status-Fenster wurde nicht sichtbar; Prozessstatus konnte nicht geprüft werden: {error}"
                ));
            }
        }
        if hwnd.is_none() && started_at.elapsed() >= LIVE_WINDOW_START_TIMEOUT {
            terminate_and_reap_child(child);
            return LiveWatchOutcome::NoWindowTimeout;
        }
        if found_at.is_some_and(|found| found.elapsed() >= LIVE_WINDOW_VISIBLE_TIMEOUT) {
            terminate_and_reap_child(child);
            return LiveWatchOutcome::UnpaintedTimeout;
        }
        thread::sleep(LIVE_WINDOW_POLL_INTERVAL);
    }
}

enum GuardOutcome {
    /// The window stays open (or the user closed it cleanly); nothing to do.
    Done,
    /// The child died shortly after opening; carries the log detail.
    Retry(String),
}

/// Phase three of the watchdog: after the window presented itself, a cold-boot
/// GPU failure can still kill the process a few frames later. A clean exit
/// code 0 means the user closed the window on purpose and must not reopen it.
fn guard_live_window_after_ready(child: &mut Child) -> GuardOutcome {
    let started_at = Instant::now();
    while started_at.elapsed() < LIVE_WINDOW_POST_OPEN_GUARD {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return GuardOutcome::Done,
            Ok(Some(status)) => {
                return GuardOutcome::Retry(format!(
                    "Live-Status-Prozess endete kurz nach dem Öffnen ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => {
                terminate_and_reap_child(child);
                return GuardOutcome::Retry(format!(
                    "Prozessstatus des Live-Status-Fensters konnte nach dem Öffnen nicht geprüft werden: {error}"
                ));
            }
        }
        thread::sleep(LIVE_WINDOW_POLL_INTERVAL);
    }
    GuardOutcome::Done
}

fn terminate_and_reap_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
    if let Ok(last) = LAST_LIVE_CHILD_PID.lock()
        && let Some(pid) = *last
    {
        // Even after the open attempt finished, the spawned child belongs to
        // this tray and must not outlive the quit.
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
    apply_taskbar_visibility(hwnd);
}

fn is_live_window_title(title: &str) -> bool {
    title == "Herdr-Nachtwächter - Live-Status" || title == "Herdr Night Watch - Live Status"
}

fn is_satellite_window_title(title: &str) -> bool {
    title == "Herdr-Nachtwächter - Limits" || title == "Herdr Night Watch - Limits"
}

fn find_satellite_window() -> Option<HWND> {
    pick_satellite_window(None, None)
}

fn find_satellite_window_for_pid(pid: u32) -> Option<HWND> {
    pick_satellite_window(Some(pid), None)
}

fn pick_satellite_window(required_pid: Option<u32>, exclude_pid: Option<u32>) -> Option<HWND> {
    let mut search = SatelliteWindowQuery {
        required_pid,
        exclude_pid,
        pick: LiveWindowPick::default(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_satellite_window_callback),
            &mut search as *mut _ as LPARAM,
        );
    }
    search.pick.hwnd
}

struct SatelliteWindowQuery {
    required_pid: Option<u32>,
    exclude_pid: Option<u32>,
    pick: LiveWindowPick,
}

unsafe extern "system" fn find_satellite_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam as *mut SatelliteWindowQuery);
        if let Some((score, area)) =
            score_satellite_window(hwnd, search.required_pid, search.exclude_pid)
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

fn score_satellite_window(
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
    score_satellite_window_facts(
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

fn score_satellite_window_facts(
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
    if !is_satellite_window_title(&facts.title) {
        return None;
    }
    let area = i64::from(facts.width.max(1)) * i64::from(facts.height.max(1));
    Some((100, area))
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

fn begin_open_spawn() -> Option<u64> {
    let Ok(mut gate) = OPEN_ATTEMPT.lock() else {
        record_open_failure("Live-Status-Öffnungssperre konnte nicht gelesen werden");
        return None;
    };
    if let Some(existing) = gate.as_ref() {
        if existing.child_pid.is_some_and(process_is_running) {
            return None;
        }
        if existing.started_at.elapsed() < OPEN_SPAWN_DEBOUNCE {
            return None;
        }
    }
    if let Some(stale) = gate.take() {
        unsafe {
            let _ = CloseHandle(stale.process_gate as HANDLE);
        }
    }
    let process_gate = match acquire_open_attempt_process_gate() {
        Ok(Some(handle)) => handle,
        Ok(None) => return None,
        Err(error) => {
            record_open_failure(&error.to_string());
            return None;
        }
    };
    let id = NEXT_OPEN_ATTEMPT_ID.fetch_add(1, Ordering::Relaxed);
    *gate = Some(OpenAttempt {
        id,
        started_at: Instant::now(),
        child_pid: None,
        process_gate,
    });
    Some(id)
}

/// Hold a named kernel object for the complete open attempt. Unlike the local
/// `OPEN_ATTEMPT` state, this also lets a secondary EXE see that another tray
/// process is still monitoring a hidden first frame.
fn acquire_open_attempt_process_gate() -> Result<Option<usize>> {
    #[cfg(test)]
    let mutex_name = std::env::var(LIVE_OPEN_ATTEMPT_MUTEX_ENV)
        .unwrap_or_else(|_| LIVE_OPEN_ATTEMPT_MUTEX.to_owned());
    #[cfg(not(test))]
    let mutex_name = LIVE_OPEN_ATTEMPT_MUTEX;
    let name: Vec<u16> = mutex_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow::anyhow!(
            "Prozessübergreifende Live-Status-Öffnungssperre konnte nicht erstellt werden"
        ));
    }
    if unsafe { GetLastError() } == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        Ok(None)
    } else {
        Ok(Some(handle as usize))
    }
}

fn active_open_child_pid() -> Option<u32> {
    let pid = OPEN_ATTEMPT
        .lock()
        .ok()?
        .as_ref()
        .and_then(|attempt| attempt.child_pid)?;
    process_is_running(pid).then_some(pid)
}

fn remember_open_child(attempt_id: u64, pid: u32) -> bool {
    if let Ok(mut gate) = OPEN_ATTEMPT.lock()
        && let Some(existing) = gate.as_mut()
        && existing.id == attempt_id
    {
        existing.child_pid = Some(pid);
        if let Ok(mut last) = LAST_LIVE_CHILD_PID.lock() {
            *last = Some(pid);
        }
        return true;
    }
    false
}

/// A hung live child without a window keeps owning the instance mutex, so
/// every later open attempt exits immediately with code 0. After a reboot the
/// graphics driver can hang exactly that first child in surface creation.
/// When there is no live window and no open attempt running, the last known
/// child is stale and terminating it frees the mutex for a fresh attempt.
fn stale_live_child_pid() -> Option<u32> {
    let pid = (*LAST_LIVE_CHILD_PID.lock().ok()?)?;
    if pid == 0 || pid == current_pid() || !process_is_running(pid) {
        return None;
    }
    process_image_is_current_exe(pid).then_some(pid)
}

fn process_image_is_current_exe(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let matches = process_handle_is_current_exe(handle);
        let _ = CloseHandle(handle);
        matches
    }
}

/// Validate the image path on one already-opened process handle so the check
/// and a later termination cannot drift apart across a recycled PID.
fn process_handle_is_current_exe(handle: HANDLE) -> bool {
    let Ok(expected) = std::env::current_exe() else {
        return false;
    };
    let expected = expected.to_string_lossy().to_lowercase();
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
    };
    if ok == 0 {
        return false;
    }
    let image = String::from_utf16_lossy(&buffer[..length as usize]);
    image.to_lowercase() == expected
}

fn finish_open_spawn(attempt_id: u64) {
    if let Ok(mut gate) = OPEN_ATTEMPT.lock()
        && gate
            .as_ref()
            .is_some_and(|attempt| attempt.id == attempt_id)
        && let Some(attempt) = gate.take()
    {
        unsafe {
            let _ = CloseHandle(attempt.process_gate as HANDLE);
        }
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
        let handle = OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if handle.is_null() {
            return;
        }
        // Terminate only the exact process that was opened: a recycled PID
        // running a different image is never killed.
        if process_handle_is_current_exe(handle) {
            let _ = TerminateProcess(handle, 0);
        }
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

fn is_ready_live_window(visible: bool, minimized: bool, width: i32, height: i32) -> bool {
    if minimized {
        return true;
    }
    visible && width >= LIVE_WINDOW_MIN_READY_WIDTH && height >= LIVE_WINDOW_MIN_READY_HEIGHT
}

/// Whether a concrete live window already presented itself. eframe creates
/// the native window hidden and reveals it after the first rendered frame, so
/// visibility is the reliable readiness signal for the tray-side watchdog.
fn live_window_is_ready_now(hwnd: HWND) -> bool {
    let (visible, minimized, width, height) = live_window_metrics(hwnd);
    is_ready_live_window(visible, minimized, width, height)
}

/// A missing timestamp means the work has never run and is therefore due
/// immediately. This avoids manufacturing a timestamp before Windows boot,
/// which can underflow `Instant` during the first minutes after a restart.
fn refresh_is_due(last: Option<Instant>, interval: Duration, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

fn apply_taskbar_visibility(hwnd: HWND) {
    let _ = taskbar::set_visible(hwnd, window_settings::live_status_in_taskbar());
    if let Some(sat_hwnd) = find_satellite_window() {
        taskbar::set_satellite_exempt(sat_hwnd, Some(hwnd));
    }
}

pub fn apply_taskbar_setting() -> Result<()> {
    let show = window_settings::live_status_in_taskbar();
    let main_hwnd = find_live_window();
    if let Some(hwnd) = main_hwnd
        && !taskbar::set_visible(hwnd, show)
    {
        anyhow::bail!("Windows konnte die Taskleistenanzeige nicht ändern");
    }
    if let Some(sat_hwnd) = find_satellite_window() {
        taskbar::set_satellite_exempt(sat_hwnd, main_hwnd);
    }
    Ok(())
}

fn record_open_failure(detail: &str) {
    record_ui_error(LIVE_STATUS_OPEN_FAILURE_EVENT, detail);
}

fn record_ui_error(event: &str, detail: &str) {
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
        "{:?};{};{}",
        SystemTime::now(),
        event,
        detail.replace(['\r', '\n', ';'], " ")
    );
}

pub fn run() -> Result<()> {
    install_panic_logger();
    let instance_mutex = match acquire_live_instance()? {
        Some(handle) => handle,
        None => {
            let hwnd = find_live_window();
            if let Some(hwnd) = hwnd {
                apply_taskbar_visibility(hwnd);
            }
            match duplicate_instance_disposition(hwnd, live_window_is_ready_now) {
                DuplicateInstanceDisposition::Activate(hwnd) => {
                    activate_live_window(hwnd);
                    return Ok(());
                }
                DuplicateInstanceDisposition::Retry => {
                    return Err(anyhow::anyhow!(
                        "Eine andere Live-Status-Instanz hält die Sperre, hat aber kein fertig gezeichnetes Fenster"
                    ));
                }
            }
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

/// A panicking live child dies with exit code 101 and, without this hook,
/// leaves no trace: the watchdog may already have reported success and the
/// Windows subsystem hides stderr. Write the panic into the shared error log
/// so the next cold boot finally names the real culprit.
fn install_panic_logger() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_ui_error(
            LIVE_STATUS_PANIC_EVENT,
            &format!("Live-Status-Panik: {info}"),
        );
        default_hook(info);
    }));
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

/// Visible working area of one monitor in desktop coordinates. Coordinates
/// can be negative when a monitor is placed left of or above the primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WorkArea {
    fn from_physical_rect(rect: RECT, pixels_per_point: f32) -> Self {
        let pixels_per_point = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            f64::from(pixels_per_point)
        } else {
            1.0
        };
        let to_points = |pixels: i32| (f64::from(pixels) / pixels_per_point).round() as i32;
        Self {
            left: to_points(rect.left),
            top: to_points(rect.top),
            right: to_points(rect.right),
            bottom: to_points(rect.bottom),
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    fn distance_squared(&self, x: i32, y: i32) -> i128 {
        // Compute deltas and squares in i128: extreme saved coordinates
        // (for example a corrupted registry value saturating to i32::MIN)
        // overflow i32 arithmetic before the distance is even compared.
        let dx = if x < self.left {
            i128::from(self.left) - i128::from(x)
        } else if x >= self.right {
            i128::from(x) - i128::from(self.right) + 1
        } else {
            0
        };
        let dy = if y < self.top {
            i128::from(self.top) - i128::from(y)
        } else if y >= self.bottom {
            i128::from(y) - i128::from(self.bottom) + 1
        } else {
            0
        };
        dx * dx + dy * dy
    }

    fn clamp_position(&self, x: i32, y: i32, width: i32, height: i32) -> [i32; 2] {
        let max_x = (self.right - width).max(self.left);
        let max_y = (self.bottom - height).max(self.top);
        [x.clamp(self.left, max_x), y.clamp(self.top, max_y)]
    }
}

/// Keep the saved live window position on a monitor that currently exists.
/// After a reboot a secondary monitor can still be asleep or unplugged while
/// its saved position would place the window off screen and invisible.
fn clamp_live_position(
    saved: Option<[f32; 2]>,
    window_size: [f32; 2],
    areas: &[WorkArea],
) -> Option<[f32; 2]> {
    let [sx, sy] = saved?;
    if !sx.is_finite() || !sy.is_finite() || areas.is_empty() {
        return None;
    }
    let x = sx.round() as i32;
    let y = sy.round() as i32;
    let width = window_size[0].round().max(1.0) as i32;
    let height = window_size[1].round().max(1.0) as i32;
    // Clamp even when the origin itself sits inside an area: a large window
    // can still stick out over the monitor edge from an inside origin.
    let area = areas
        .iter()
        .find(|area| area.contains(x, y))
        .or_else(|| areas.iter().min_by_key(|area| area.distance_squared(x, y)))
        .expect("areas is not empty");
    let [cx, cy] = area.clamp_position(x, y, width, height);
    Some([cx as f32, cy as f32])
}

struct MonitorAreaCollector {
    areas: Vec<WorkArea>,
}

unsafe extern "system" fn monitor_area_callback(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    unsafe {
        let collector = &mut *(lparam as *mut MonitorAreaCollector);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let mut dpi_x = 96;
            let mut dpi_y = 96;
            let pixels_per_point =
                if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) >= 0
                    && dpi_x > 0
                {
                    dpi_x as f32 / 96.0
                } else {
                    1.0
                };
            collector
                .areas
                .push(WorkArea::from_physical_rect(info.rcWork, pixels_per_point));
        }
    }
    1
}

fn monitor_work_areas() -> Vec<WorkArea> {
    let mut collector = MonitorAreaCollector { areas: Vec::new() };
    unsafe {
        let _ = EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(monitor_area_callback),
            &mut collector as *mut MonitorAreaCollector as LPARAM,
        );
    }
    collector.areas
}

fn live_viewport_builder(
    scale: f32,
    position: Option<[f32; 2]>,
    language: Language,
) -> egui::ViewportBuilder {
    let mut viewport = egui::ViewportBuilder::default()
        .with_taskbar(window_settings::live_status_in_taskbar())
        // Do not set `visible=false` here. eframe already creates native
        // windows hidden and reveals them after their first rendered frame.
        // Marking the egui viewport itself hidden prevents `App::ui` from
        // running, so the native window can only expose an empty white surface.
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
        .with_title(match language {
            Language::German => "Herdr-Nachtwächter - Live-Status",
            Language::English => "Herdr Night Watch - Live Status",
        });
    if let Some(position) = position {
        viewport = viewport.with_position(position);
    }
    viewport
}

fn run_window() -> Result<()> {
    let scale = window_settings::live_status_scale();
    let saved_position = window_settings::live_status_position();
    let viewport = live_viewport_builder(scale, None, Language::current());
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        // eframe creates winit's event loop before invoking this hook. On
        // Windows that is also when the process becomes per-monitor DPI
        // aware, so GetDpiForMonitor and the saved egui position use the same
        // logical coordinate system here.
        window_builder: Some(Box::new(move |viewport| {
            let clamped_position = clamp_live_position(
                saved_position,
                [DESIGN_WIDTH * scale, DESIGN_HEIGHT * scale],
                &monitor_work_areas(),
            );
            if let Some(position) = clamped_position {
                if saved_position != Some(position) {
                    // Self-healing: persist the corrected position so the
                    // next start does not repeat the clamp.
                    let _ = window_settings::set_live_status_position(position);
                }
                viewport.with_position(position)
            } else {
                viewport
            }
        })),
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
    last_refresh: Option<Instant>,
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
    last_weather_fetch: Option<Instant>,
    last_weather_location_check: Option<Instant>,
    transparency: Option<u8>,
    window_level: window_settings::WindowLevel,
    taskbar_visible: Option<bool>,
    window_drag_started: bool,
    resize_drag: Option<(egui::Pos2, f32, egui::Vec2)>,
    resize_preview_scale: Option<f32>,
    resize_preview_window_size: Option<egui::Vec2>,
    context_menu_pos: Option<egui::Pos2>,
    clock_visible: bool,
    clock_second_hand_visible: bool,
    repaint_interval_ms: u32,
    scale: f32,
    last_saved_position: Option<[f32; 2]>,
    owner_pid: Option<u32>,
    quota_snapshot: QuotaSnapshot,
    quota_rx: Receiver<QuotaSnapshot>,
    quota_tx: Sender<QuotaSnapshot>,
    quota_checking: bool,
    last_quota_fetch: Option<Instant>,
    quota_open: bool,
    quota_docked: bool,
    quota_pos: Option<[f32; 2]>,
    quota_drag_active: bool,
    quota_resize_drag: Option<(egui::Pos2, f32, egui::Vec2)>,
    satellite_configured_hwnd: Option<HWND>,
    quota_show_glm: bool,
    quota_show_agi: bool,
    quota_show_codex: bool,
    quota_show_claude: bool,
    quota_color_glm: u8,
    quota_color_agi: u8,
    quota_color_codex: u8,
    quota_color_claude: u8,
    quota_settings_open: bool,
    config_tab: ConfigTab,
    corner_radius: u8,
    pomodoro_total_seconds: f32,
    pomodoro_remaining_seconds: f32,
    pomodoro_running: bool,
    pomodoro_last_tick: Option<Instant>,
    pomodoro_drag_active: bool,
    main_hwnd: Option<HWND>,
    satellite_hwnd: Option<HWND>,
    last_position_persist: Option<Instant>,
    pending_persist_position: Option<[f32; 2]>,
    last_settings_poll: Option<Instant>,
    last_docked_state: Option<bool>,
    last_synced_radius: Option<u8>,
    first_frame_painted: bool,
    satellite_frames_painted: usize,
    satellite_opacity_applied: Option<(HWND, u8)>,
    last_synced_sat_height: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigTab {
    Providers,
    Window,
    System,
}

impl Drop for LiveStatusApp {
    fn drop(&mut self) {
        let _ = self.media_command_tx.send(MediaCommand::Shutdown);
    }
}

impl LiveStatusApp {
    fn new(scale: f32, owner_pid: Option<u32>) -> Self {
        let (status_tx, status_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (metrics_tx, metrics_rx) = mpsc::channel();
        let (media_command_tx, media_rx) = media::spawn_worker();
        let (weather_tx, weather_rx) = mpsc::channel();
        let (quota_tx, quota_rx) = mpsc::channel();
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
            last_refresh: None,
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
            last_weather_fetch: None,
            last_weather_location_check: None,
            transparency: None,
            window_level: window_settings::WindowLevel::current(),
            taskbar_visible: None,
            window_drag_started: false,
            resize_drag: None,
            resize_preview_scale: None,
            resize_preview_window_size: None,
            context_menu_pos: None,
            clock_visible: window_settings::clock_visible(),
            clock_second_hand_visible: window_settings::clock_second_hand_visible(),
            repaint_interval_ms: window_settings::live_status_repaint_interval_ms(),
            scale,
            last_saved_position: window_settings::live_status_position(),
            owner_pid,
            quota_snapshot: QuotaSnapshot::measured_baseline(),
            quota_rx,
            quota_tx,
            quota_checking: false,
            last_quota_fetch: None,
            quota_open: window_settings::live_status_quota_open(),
            quota_docked: window_settings::live_status_quota_docked(),
            quota_pos: window_settings::live_status_quota_position(),
            quota_drag_active: false,
            quota_resize_drag: None,
            satellite_configured_hwnd: None,
            quota_show_glm: window_settings::live_status_quota_show_glm(),
            quota_show_agi: window_settings::live_status_quota_show_agi(),
            quota_show_codex: window_settings::live_status_quota_show_codex(),
            quota_show_claude: window_settings::live_status_quota_show_claude(),
            quota_color_glm: window_settings::live_status_quota_color_glm(),
            quota_color_agi: window_settings::live_status_quota_color_agi(),
            quota_color_codex: window_settings::live_status_quota_color_codex(),
            quota_color_claude: window_settings::live_status_quota_color_claude(),
            quota_settings_open: window_settings::live_status_quota_settings_open(),
            config_tab: ConfigTab::Window,
            corner_radius: window_settings::live_status_corner_radius(),
            pomodoro_total_seconds: 0.0,
            pomodoro_remaining_seconds: 0.0,
            pomodoro_running: false,
            pomodoro_last_tick: None,
            pomodoro_drag_active: false,
            main_hwnd: None,
            satellite_hwnd: None,
            last_position_persist: None,
            pending_persist_position: None,
            last_settings_poll: None,
            last_docked_state: None,
            last_synced_radius: None,
            first_frame_painted: false,
            satellite_frames_painted: 0,
            satellite_opacity_applied: None,
            last_synced_sat_height: None,
        }
    }

    pub fn sync_corner_regions(&self) {
        let is_docked = self.quota_open && self.quota_docked;
        let r = self.corner_radius;
        let scale = self.scale;
        let op = self.opacity();
        if let Some(main_hwnd) = self.main_hwnd {
            window_chrome::apply_corner_preference(main_hwnd, r, false, is_docked, scale);
            window_chrome::ensure_window_chrome_synced(main_hwnd, op, self.window_level);
            let tb = window_settings::live_status_in_taskbar();
            if !taskbar::is_taskbar_style_synced(main_hwnd, tb) {
                let _ = taskbar::set_visible(main_hwnd, tb);
            }
        }
        if let Some(sat_hwnd) = self.satellite_hwnd {
            let sat_h = self.last_synced_sat_height.unwrap_or(314.0);
            window_chrome::sync_satellite_size(sat_hwnd, sat_h, self.main_hwnd, r, is_docked);
            window_chrome::ensure_window_chrome_synced(sat_hwnd, op, self.window_level);
            if !taskbar::is_satellite_exempt_synced(sat_hwnd) {
                taskbar::set_satellite_exempt(sat_hwnd, self.main_hwnd);
            }
        }
    }

    pub fn apply_window_level(&mut self, ctx: &egui::Context, level: window_settings::WindowLevel) {
        let _ = level.set();
        self.window_level = level;
        let egui_level = window_chrome::window_level(level);

        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::WindowLevel(egui_level),
        );
        if self.quota_open {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of("live_status_quota_viewport"),
                egui::ViewportCommand::WindowLevel(egui_level),
            );
        }

        let op = self.opacity();
        if let Some(main_hwnd) = self.main_hwnd {
            window_chrome::ensure_window_chrome_synced(main_hwnd, op, level);
        } else if let Some(main_hwnd) = find_live_window_for_pid(current_pid()) {
            self.main_hwnd = Some(main_hwnd);
            window_chrome::ensure_window_chrome_synced(main_hwnd, op, level);
        }

        if let Some(sat_hwnd) = self.satellite_hwnd {
            window_chrome::ensure_window_chrome_synced(sat_hwnd, op, level);
        } else if self.quota_open {
            if let Some(sat_hwnd) = find_satellite_window_for_pid(current_pid()) {
                self.satellite_hwnd = Some(sat_hwnd);
                window_chrome::ensure_window_chrome_synced(sat_hwnd, op, level);
            }
        }
    }

    pub fn set_corner_radius(&mut self, radius: u8) {
        let r = window_settings::clamp_corner_radius(radius);
        self.corner_radius = r;
        let _ = window_settings::set_live_status_corner_radius(r);
        self.sync_corner_regions();
    }

    pub fn transparency(&self) -> u8 {
        self.transparency.unwrap_or_else(window_settings::transparency)
    }

    pub fn opacity(&self) -> u8 {
        100 - self.transparency()
    }

    pub fn apply_transparency(&mut self, transparency: u8) {
        let transparency = window_settings::clamp_transparency(transparency);
        self.transparency = Some(transparency);
        self.satellite_opacity_applied = None;
        let _ = window_settings::set_transparency(transparency);
        if let Some(main_hwnd) = self.main_hwnd {
            window_chrome::apply_window_transparency_hwnd(main_hwnd, transparency);
            window_chrome::apply_window_level_hwnd(main_hwnd, self.window_level);
        } else {
            window_chrome::apply_window_transparency(transparency, live_title(self.language));
        }
        if let Some(sat_hwnd) = self.satellite_hwnd {
            window_chrome::apply_window_transparency_hwnd(sat_hwnd, transparency);
            window_chrome::apply_window_level_hwnd(sat_hwnd, self.window_level);
        } else {
            let sat_title = match self.language {
                Language::German => "Herdr-Nachtwächter - Limits",
                Language::English => "Herdr Night Watch - Limits",
            };
            window_chrome::apply_window_transparency(transparency, sat_title);
        }
    }

    #[allow(dead_code)]
    pub fn apply_opacity(&mut self, opacity: u8) {
        if opacity <= 100 {
            self.apply_transparency(100 - opacity);
        } else {
            self.apply_transparency(0);
        }
    }

    pub fn provider_color_idx(&self, id: ProviderId) -> u8 {
        match id {
            ProviderId::Glm => self.quota_color_glm,
            ProviderId::Agy => self.quota_color_agi,
            ProviderId::Codex => self.quota_color_codex,
            ProviderId::Claude => self.quota_color_claude,
        }
    }

    pub fn provider_color(&self, id: ProviderId) -> egui::Color32 {
        provider_palette_color(self.provider_color_idx(id))
    }

    pub fn set_provider_color(&mut self, id: ProviderId, color_idx: u8) {
        let idx = color_idx.min(9);
        match id {
            ProviderId::Glm => {
                self.quota_color_glm = idx;
                let _ = window_settings::set_live_status_quota_color_glm(idx);
            }
            ProviderId::Agy => {
                self.quota_color_agi = idx;
                let _ = window_settings::set_live_status_quota_color_agi(idx);
            }
            ProviderId::Codex => {
                self.quota_color_codex = idx;
                let _ = window_settings::set_live_status_quota_color_codex(idx);
            }
            ProviderId::Claude => {
                self.quota_color_claude = idx;
                let _ = window_settings::set_live_status_quota_color_claude(idx);
            }
        }
    }

    fn refresh(&mut self) {
        if self.checking
            || !refresh_is_due(self.last_refresh, Duration::from_secs(3), Instant::now())
        {
            return;
        }
        self.checking = true;
        self.last_refresh = Some(Instant::now());
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
        if refresh_is_due(
            self.last_weather_location_check,
            Duration::from_secs(5),
            Instant::now(),
        ) {
            self.last_weather_location_check = Some(Instant::now());
            let location = weather::current_location();
            if location != self.weather_location {
                self.weather_location = location;
                self.weather_reading = None;
                self.last_weather_fetch = None;
            }
        }
        if self.weather_checking
            || !refresh_is_due(
                self.last_weather_fetch,
                Duration::from_secs(600),
                Instant::now(),
            )
        {
            return;
        }
        self.weather_checking = true;
        self.last_weather_fetch = Some(Instant::now());
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

    fn refresh_quota(&mut self) {
        if self.quota_checking
            || !refresh_is_due(
                self.last_quota_fetch,
                Duration::from_secs(45),
                Instant::now(),
            )
        {
            return;
        }
        self.quota_checking = true;
        self.last_quota_fetch = Some(Instant::now());
        let sender = self.quota_tx.clone();
        let snapshot = self.quota_snapshot.clone();
        thread::spawn(move || {
            let updated = crate::quota::fetch_live_snapshot(&snapshot);
            let _ = sender.send(updated);
        });
    }

    fn collect_quota(&mut self) {
        while let Ok(snapshot) = self.quota_rx.try_recv() {
            self.quota_checking = false;
            self.quota_snapshot = snapshot;
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
                    self.last_refresh = None;
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

        let mut hwnds_changed = false;
        if self.main_hwnd.is_none() {
            self.main_hwnd = find_live_window_for_pid(current_pid());
            if let Some(hwnd) = self.main_hwnd {
                hwnds_changed = true;
                let current_taskbar_visibility = window_settings::live_status_in_taskbar();
                let _ = taskbar::set_visible(hwnd, current_taskbar_visibility);
                self.taskbar_visible = Some(current_taskbar_visibility);
            }
        }
        if self.quota_open {
            if self.satellite_hwnd.is_none() {
                self.satellite_hwnd = find_satellite_window_for_pid(current_pid());
                if self.satellite_hwnd.is_some() {
                    hwnds_changed = true;
                }
            }
            if let Some(sat_hwnd) = self.satellite_hwnd {
                if self.satellite_configured_hwnd != Some(sat_hwnd)
                    || !taskbar::is_satellite_exempt_synced(sat_hwnd)
                {
                    let main_hwnd = self.main_hwnd;
                    taskbar::set_satellite_exempt(sat_hwnd, main_hwnd);
                    self.satellite_configured_hwnd = Some(sat_hwnd);
                    hwnds_changed = true;
                }
            }
        } else {
            if self.satellite_hwnd.is_some() {
                hwnds_changed = true;
            }
            self.satellite_hwnd = None;
            self.satellite_configured_hwnd = None;
            self.satellite_frames_painted = 0;
            self.satellite_opacity_applied = None;
            self.last_synced_sat_height = None;
        }

        if hwnds_changed {
            self.sync_corner_regions();
        }

        let now = Instant::now();
        let settings_poll_due = self
            .last_settings_poll
            .map_or(true, |t| now.duration_since(t) >= Duration::from_millis(500));
        if settings_poll_due {
            self.last_settings_poll = Some(now);
            if self.first_frame_painted {
                let current_trans = window_settings::transparency();
                if self.transparency != Some(current_trans) {
                    self.apply_transparency(current_trans);
                } else {
                    let op = 100 - current_trans;
                    if let Some(main_hwnd) = self.main_hwnd {
                        window_chrome::ensure_window_chrome_synced(main_hwnd, op, self.window_level);
                    }
                    if let Some(sat_hwnd) = self.satellite_hwnd {
                        window_chrome::ensure_window_chrome_synced(sat_hwnd, op, self.window_level);
                    }
                }
            }
            let current_window_level = window_settings::WindowLevel::current();
            if current_window_level != self.window_level {
                self.apply_window_level(ui.ctx(), current_window_level);
            }
            let current_taskbar_visibility = window_settings::live_status_in_taskbar();
            if let Some(hwnd) = self.main_hwnd {
                if self.taskbar_visible != Some(current_taskbar_visibility)
                    || !taskbar::is_taskbar_style_synced(hwnd, current_taskbar_visibility)
                {
                    let _ = taskbar::set_visible(hwnd, current_taskbar_visibility);
                    self.taskbar_visible = Some(current_taskbar_visibility);
                }
            }
            self.quota_show_glm = window_settings::live_status_quota_show_glm();
            self.quota_show_agi = window_settings::live_status_quota_show_agi();
            self.quota_show_codex = window_settings::live_status_quota_show_codex();
            self.quota_show_claude = window_settings::live_status_quota_show_claude();
            self.quota_color_glm = window_settings::live_status_quota_color_glm();
            self.quota_color_agi = window_settings::live_status_quota_color_agi();
            self.quota_color_codex = window_settings::live_status_quota_color_codex();
            self.quota_color_claude = window_settings::live_status_quota_color_claude();
            let current_radius = window_settings::live_status_corner_radius();
            if current_radius != self.corner_radius {
                self.set_corner_radius(current_radius);
            }
            let current_quota_open = window_settings::live_status_quota_open();
            if current_quota_open != self.quota_open {
                self.quota_open = current_quota_open;
            }
            let current_quota_docked = window_settings::live_status_quota_docked();
            if current_quota_docked != self.quota_docked {
                self.quota_docked = current_quota_docked;
            }
            let current_quota_settings_open = window_settings::live_status_quota_settings_open();
            if current_quota_settings_open != self.quota_settings_open {
                self.quota_settings_open = current_quota_settings_open;
                self.last_synced_sat_height = None;
            }
        }

        if self.quota_open && self.satellite_frames_painted >= 2 {
            let op = self.opacity();
            if let Some(sat_hwnd) = self.satellite_hwnd {
                if self.satellite_opacity_applied != Some((sat_hwnd, op)) {
                    window_chrome::apply_window_opacity_hwnd(sat_hwnd, op);
                    self.satellite_opacity_applied = Some((sat_hwnd, op));
                    self.sync_corner_regions();
                }
            }
        }
        self.collect_results();
        self.collect_actions();
        self.collect_metrics();
        self.collect_media();
        self.collect_weather();
        self.collect_quota();
        self.refresh();
        self.refresh_weather();
        self.refresh_quota();
        if self.pomodoro_running {
            let now = Instant::now();
            if let Some(last) = self.pomodoro_last_tick {
                let delta = now.duration_since(last).as_secs_f32();
                self.pomodoro_remaining_seconds = (self.pomodoro_remaining_seconds - delta).max(0.0);
                if self.pomodoro_remaining_seconds <= 0.0 {
                    self.pomodoro_remaining_seconds = 0.0;
                    self.pomodoro_total_seconds = 0.0;
                    self.pomodoro_running = false;
                    #[cfg(windows)]
                    unsafe {
                        unsafe extern "system" {
                            fn MessageBeep(u_type: u32) -> i32;
                        }
                        MessageBeep(0x00000040);
                    }
                    self.toast = Some(Toast {
                        message: self
                            .language
                            .text(
                                "⏱ Pomodoro abgeschlossen! Zeit für eine Pause.",
                                "⏱ Pomodoro finished! Time for a break.",
                            )
                            .into(),
                        color: egui::Color32::from_rgb(239, 68, 68),
                        expires_at: now + Duration::from_secs(6),
                    });
                }
            }
            self.pomodoro_last_tick = Some(now);
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else {
            self.pomodoro_last_tick = None;
        }
        if self.pomodoro_drag_active {
            ui.ctx().request_repaint();
        }
        let is_docked = self.quota_open && self.quota_docked;
        if self.last_docked_state != Some(is_docked) || self.last_synced_radius != Some(self.corner_radius) {
            self.last_docked_state = Some(is_docked);
            self.last_synced_radius = Some(self.corner_radius);
            self.sync_corner_regions();
        }
        window_chrome::default_gradient(ui.painter(), ui.max_rect());
        // Register the controls now, then paint them after the content. This
        // deliberately gives their opaque hood precedence over the weather
        // line and any clock artwork that reaches into the title area.
        let controls = window_controls(ui, self.language, self.window_level);
        if controls.log_clicked {
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
        if controls.level_clicked {
            let next_level = self.window_level.next();
            self.apply_window_level(ui.ctx(), next_level);
            self.toast = Some(Toast {
                message: window_level_message(next_level, self.language).into(),
                color: ACCENT,
                expires_at: Instant::now() + Duration::from_secs(3),
            });
        }
        if controls.minimize_clicked {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            if self.quota_open {
                ui.ctx().send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("live_status_quota_viewport"),
                    egui::ViewportCommand::Minimized(true),
                );
            }
        }
        if controls.close_clicked {
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
                                let (response, clock_interaction) = moon_icon(
                                    ui,
                                    moon.color,
                                    MOON_ICON_DIAMETER,
                                    gradient_rect,
                                    moon.temperature_c,
                                    moon.weather_symbol,
                                    moon.phase,
                                    self.clock_visible,
                                    self.clock_second_hand_visible,
                                    self.pomodoro_total_seconds,
                                    self.pomodoro_remaining_seconds,
                                    self.pomodoro_running,
                                );
                                let tooltip = if self.clock_visible {
                                    if self.pomodoro_total_seconds > 0.0 {
                                        let mins = (self.pomodoro_remaining_seconds / 60.0).ceil() as u32;
                                        let total_mins = (self.pomodoro_total_seconds / 60.0).round() as u32;
                                        let state_label = if self.pomodoro_running {
                                            self.language.text("Läuft", "Running")
                                        } else {
                                            self.language.text("Pausiert", "Paused")
                                        };
                                        format!(
                                            "{}\n⏱ Pomodoro: {mins} / {total_mins} min ({state_label})\n{}",
                                            moon.tooltip,
                                            self.language.text(
                                                "Mitte: Start/Pause · Ziehen: Einstellen · Doppelklick: Löschen",
                                                "Center: Start/Pause · Drag: Set time · Double click: Reset",
                                            ),
                                        )
                                    } else {
                                        format!(
                                            "{}\n{}",
                                            moon.tooltip,
                                            self.language.text(
                                                "⏱ Pomodoro: Auf Ziffernblatt ziehen (bis 60 Min.) oder Mitte klicken (25 Min.) · Doppelklick: Löschen",
                                                "⏱ Pomodoro: Drag on dial (up to 60 min) or click center (25 min) · Double click: Reset",
                                            ),
                                        )
                                    }
                                } else {
                                    moon.tooltip.to_string()
                                };
                                let _ = response.on_hover_text(tooltip);

                                match clock_interaction {
                                    ClockInteraction::TogglePomodoro => {
                                        if self.pomodoro_running {
                                            self.pomodoro_running = false;
                                        } else {
                                            if self.pomodoro_remaining_seconds <= 0.0 {
                                                self.pomodoro_total_seconds = 25.0 * 60.0;
                                                self.pomodoro_remaining_seconds = 25.0 * 60.0;
                                            }
                                            self.pomodoro_running = true;
                                            self.pomodoro_last_tick = Some(Instant::now());
                                        }
                                    }
                                    ClockInteraction::SetPomodoroSeconds(seconds) => {
                                        self.pomodoro_total_seconds = seconds;
                                        self.pomodoro_remaining_seconds = seconds;
                                        self.pomodoro_running = false;
                                    }
                                    ClockInteraction::ResetPomodoro => {
                                        self.pomodoro_total_seconds = 0.0;
                                        self.pomodoro_remaining_seconds = 0.0;
                                        self.pomodoro_running = false;
                                    }
                                    ClockInteraction::TriggerNightAction => {
                                        if !self.action_in_progress {
                                            self.run_action(action);
                                        }
                                    }
                                    ClockInteraction::None => {}
                                }

                                if let Some(seconds_remaining) = countdown_seconds {
                                    ui.add_space(11.0);
                                    ui.label(
                                        egui::RichText::new(format_countdown(seconds_remaining))
                                            .size(11.0)
                                            .strong()
                                            .color(RED),
                                    );
                                }
                            },
                        );
                    },
                );
                ui.add_space(KPI_ROW_TOP_GAP);
                let metrics_text_right = system_metrics_row(ui, self.metrics, self.language);
                ui.add_space(4.0);
                let (seek_action, toggle_quota) = media_info_row(
                    ui,
                    self.media_snapshot.as_ref(),
                    metrics_text_right,
                    &self.quota_snapshot,
                    self.quota_open,
                    self.language,
                    self.quota_show_glm,
                    self.quota_show_agi,
                    self.quota_show_codex,
                    self.quota_show_claude,
                );
                if let Some(position) = seek_action {
                    let _ = self.media_command_tx.send(MediaCommand::Seek(position));
                }
                if toggle_quota {
                    self.quota_open = !self.quota_open;
                    let _ = window_settings::set_live_status_quota_open(self.quota_open);
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
        controls.paint(ui.painter(), self.window_level);
        if self.quota_open {
            render_quota_satellite_window(self, ui.ctx());
        }
        // Each repaint has a small random black-flash chance on the OpenGL
        // surface (see window_settings::REPAINT_INTERVAL_MS_VALUES), so the
        // cadence is a user setting instead of a constant.
        let repaint_delay = if self.pomodoro_running {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(u64::from(self.repaint_interval_ms))
        };
        if !self.first_frame_painted {
            self.first_frame_painted = true;
            ui.ctx().request_repaint();
        }
        ui.ctx().request_repaint_after(repaint_delay);
    }
}

fn persist_window_position(app: &mut LiveStatusApp, ctx: &egui::Context) {
    let primary_down = ctx.input(|input| input.pointer.primary_down());
    let current_pos = ctx.input(|input| {
        input
            .viewport()
            .outer_rect
            .map(|rect| [rect.min.x.round(), rect.min.y.round()])
    });

    if primary_down || app.window_drag_started {
        if let Some(pos) = current_pos {
            app.pending_persist_position = Some(pos);
            app.last_position_persist = Some(Instant::now());
        }
        return;
    }

    if let Some(pending) = app.pending_persist_position {
        let is_settled = app
            .last_position_persist
            .map_or(true, |t| t.elapsed() >= Duration::from_millis(400));
        if is_settled {
            if Some(pending) != app.last_saved_position {
                let _ = window_settings::set_live_status_position(pending);
                app.last_saved_position = Some(pending);
            }
            app.pending_persist_position = None;
        }
    } else if let Some(pos) = current_pos {
        if Some(pos) != app.last_saved_position {
            app.pending_persist_position = Some(pos);
            app.last_position_persist = Some(Instant::now());
        }
    }
}

fn provider_card_height(quota: &ProviderQuota) -> f32 {
    let has_5h = quota.five_hour_percent.is_some() || quota.five_hour_reset.is_some();
    let has_wk = quota.week_percent.is_some() || quota.week_reset.is_some();
    let count = match (has_5h, has_wk) {
        (true, true) => 2,
        (false, false) => 1,
        _ => 1,
    };
    if count == 2 {
        104.0
    } else {
        68.0
    }
}

fn quota_satellite_design_height(visible_providers: &[ProviderQuota], settings_open: bool) -> f32 {
    if settings_open {
        return 314.0;
    }
    if visible_providers.is_empty() {
        return 38.0 + 42.0 + 8.0;
    }
    let total_cards: f32 = visible_providers.iter().map(provider_card_height).sum();
    let gaps = (visible_providers.len() - 1) as f32 * 6.0;
    38.0 + total_cards + gaps + 8.0
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
    if app.quota_open {
        let quota_viewport_id = egui::ViewportId::from_hash_of("live_status_quota_viewport");
        let visible_providers: Vec<ProviderQuota> = app
            .quota_snapshot
            .providers
            .iter()
            .filter(|p| match p.id {
                ProviderId::Glm => app.quota_show_glm,
                ProviderId::Agy => app.quota_show_agi,
                ProviderId::Codex => app.quota_show_codex,
                ProviderId::Claude => app.quota_show_claude,
            })
            .cloned()
            .collect();
        let sat_height = quota_satellite_design_height(&visible_providers, app.quota_settings_open);
        app.last_synced_sat_height = Some(sat_height);
        ctx.send_viewport_cmd_to(
            quota_viewport_id,
            egui::ViewportCommand::InnerSize(egui::vec2(
                DESIGN_WIDTH * scale / previous_zoom,
                sat_height * scale / previous_zoom,
            )),
        );
        if app.quota_docked {
            if let Some(main) = ctx.input(|i| i.viewport().outer_rect) {
                let dock_overlap = 1.0;
                ctx.send_viewport_cmd_to(
                    quota_viewport_id,
                    egui::ViewportCommand::OuterPosition(egui::pos2(
                        main.min.x,
                        main.max.y - dock_overlap,
                    )),
                );
            }
        }
        app.sync_corner_regions();
    }
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
                if app.quota_open {
                    let quota_viewport_id =
                        egui::ViewportId::from_hash_of("live_status_quota_viewport");
                    let sat_native_preview_size = egui::vec2(
                        native_preview_size.x,
                        176.0 * (native_preview_size.x / DESIGN_WIDTH),
                    );
                    ui.ctx().send_viewport_cmd_to(
                        quota_viewport_id,
                        egui::ViewportCommand::InnerSize(sat_native_preview_size),
                    );
                }
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
    for offset in [0.0, 4.0, 8.0] {
        ui.painter().line_segment(
            [
                egui::pos2(grip.right() - 7.0 - offset, grip.bottom() - 4.0),
                egui::pos2(grip.right() - 4.0, grip.bottom() - 7.0 - offset),
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
        let r = app.corner_radius;
        let is_docked = app.quota_open && app.quota_docked;
        let preview_corners = if is_docked {
            egui::CornerRadius {
                nw: r,
                ne: r,
                sw: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::same(r)
        };
        let preview_inner_corners = if is_docked {
            egui::CornerRadius {
                nw: r.saturating_sub(2),
                ne: r.saturating_sub(2),
                sw: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::same(r.saturating_sub(2))
        };
        ui.painter()
            .rect_filled(preview_rect, preview_corners, preview_fill);
        ui.painter().rect_stroke(
            preview_rect,
            preview_corners,
            egui::Stroke::new(1.0, preview_color),
            egui::StrokeKind::Inside,
        );
        ui.painter().rect_stroke(
            preview_rect.shrink(4.0),
            preview_inner_corners,
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

fn is_live_context_menu_position(rect: egui::Rect, position: egui::Pos2) -> bool {
    let completion_switch = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 86.0, rect.top() + 12.0),
        egui::pos2(rect.left() + 168.0, rect.top() + 50.0),
    );
    let warning_seconds = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 176.0, rect.top() + 12.0),
        egui::pos2(rect.left() + 244.0, rect.top() + 50.0),
    );
    let control_hood = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 138.0, rect.top() + 1.0),
        egui::pos2(rect.right() - 3.0, rect.top() + 30.0),
    );
    rect.contains(position)
        && !completion_switch.contains(position)
        && !warning_seconds.contains(position)
        && !control_hood.contains(position)
        && !weather_control_rect(rect).contains(position)
        && !resize_grip_rect(rect).contains(position)
}

// Radio entries for the context menu; the interval values must stay in
// sync with window_settings::REPAINT_INTERVAL_MS_VALUES.
const REPAINT_INTERVAL_CHOICES: [(u32, &str, &str); 3] = [
    (250, "Flüssig (250 ms)", "Smooth (250 ms)"),
    (500, "Mittel (500 ms)", "Medium (500 ms)"),
    (1000, "Sekündlich (1000 ms)", "Once per second (1000 ms)"),
];

fn context_menu_should_close_after_selection(
    reset_requested: bool,
    clock_visibility_changed: bool,
    second_hand_changed: bool,
    repaint_interval_changed: bool,
) -> bool {
    reset_requested || clock_visibility_changed || second_hand_changed || repaint_interval_changed
}

fn render_palette_swatches(
    painter: &egui::Painter,
    ui: &egui::Ui,
    origin: egui::Pos2,
    selected_idx: u8,
    id_salt: &str,
    language: Language,
) -> Option<u8> {
    let circle_radius = 5.5;
    let diameter = circle_radius * 2.0;
    let gap = 4.0;
    let mut chosen = None;

    for (i, def) in PROVIDER_PASTEL_PALETTE.iter().enumerate() {
        let center = egui::pos2(
            origin.x + circle_radius + (i as f32) * (diameter + gap),
            origin.y + circle_radius,
        );
        let swatch_rect = egui::Rect::from_center_size(center, egui::vec2(diameter + 3.0, diameter + 3.0));
        let resp = ui.interact(
            swatch_rect,
            ui.make_persistent_id((id_salt, i)),
            egui::Sense::click(),
        );

        let is_selected = (selected_idx as usize) == i;
        let is_hovered = resp.hovered();

        if is_hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let name = language.text(def.name_de, def.name_en);
        let resp = resp.on_hover_text(name);

        painter.circle_filled(center, circle_radius, def.color);

        if is_selected {
            painter.circle_stroke(
                center,
                circle_radius + 1.6,
                egui::Stroke::new(1.6, egui::Color32::WHITE),
            );
        } else if is_hovered {
            painter.circle_stroke(
                center,
                circle_radius + 1.2,
                egui::Stroke::new(1.2, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
            );
        } else {
            painter.circle_stroke(
                center,
                circle_radius,
                egui::Stroke::new(0.6, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 100)),
            );
        }

        if resp.clicked() {
            chosen = Some(i as u8);
        }
    }

    chosen
}

fn context_menu_swatches(
    ui: &mut egui::Ui,
    selected_idx: u8,
    id_salt: &str,
    language: Language,
) -> Option<u8> {
    let circle_radius = 5.5;
    let diameter = circle_radius * 2.0;
    let gap = 4.0;
    let total_width = 10.0 * diameter + 9.0 * gap;
    let total_height = diameter + 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_width, total_height), egui::Sense::hover());
    render_palette_swatches(
        ui.painter(),
        ui,
        egui::pos2(rect.left(), rect.top() + 2.0),
        selected_idx,
        id_salt,
        language,
    )
}

fn render_option_pill(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    label: &str,
    is_active: bool,
    id_salt: &str,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(id_salt), egui::Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (bg, stroke_color, text_color) = if is_active {
        (
            egui::Color32::from_rgba_unmultiplied(59, 130, 246, 70),
            egui::Color32::from_rgb(96, 165, 250),
            egui::Color32::WHITE,
        )
    } else if hovered {
        (
            egui::Color32::from_rgba_unmultiplied(40, 55, 80, 140),
            egui::Color32::from_rgba_unmultiplied(120, 140, 180, 120),
            egui::Color32::from_rgb(226, 232, 240),
        )
    } else {
        (
            egui::Color32::from_rgba_unmultiplied(20, 28, 45, 120),
            egui::Color32::from_rgba_unmultiplied(65, 82, 115, 70),
            egui::Color32::from_rgb(148, 163, 184),
        )
    };
    painter.rect_filled(rect, egui::CornerRadius::same(4), bg);
    painter.rect_stroke(rect, egui::CornerRadius::same(4), egui::Stroke::new(1.0, stroke_color), egui::StrokeKind::Inside);
    let galley = painter.layout_no_wrap(label.into(), egui::FontId::proportional(10.5), text_color);
    painter.galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    resp.clicked()
}

fn render_toggle_row(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    label: &str,
    is_checked: bool,
    enabled: bool,
    id_salt: &str,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(id_salt), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let hovered = enabled && resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if hovered {
        painter.rect_filled(rect, egui::CornerRadius::same(4), egui::Color32::from_rgba_unmultiplied(30, 41, 65, 120));
    }
    let box_size = 14.0;
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - box_size / 2.0),
        egui::vec2(box_size, box_size),
    );
    let (box_bg, box_stroke) = if !enabled {
        (egui::Color32::from_rgba_unmultiplied(30, 40, 60, 60), egui::Color32::from_rgba_unmultiplied(80, 90, 110, 60))
    } else if is_checked {
        (egui::Color32::from_rgb(22, 163, 74), egui::Color32::from_rgb(34, 197, 94))
    } else if hovered {
        (egui::Color32::from_rgba_unmultiplied(35, 48, 72, 180), egui::Color32::from_rgb(148, 163, 184))
    } else {
        (egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140), egui::Color32::from_rgba_unmultiplied(80, 100, 140, 100))
    };
    painter.rect_filled(box_rect, egui::CornerRadius::same(3), box_bg);
    painter.rect_stroke(box_rect, egui::CornerRadius::same(3), egui::Stroke::new(1.0, box_stroke), egui::StrokeKind::Inside);
    if is_checked {
        let c = box_rect.center();
        let check_color = egui::Color32::WHITE;
        painter.line_segment([egui::pos2(c.x - 3.5, c.y), egui::pos2(c.x - 1.0, c.y + 2.5)], egui::Stroke::new(1.5, check_color));
        painter.line_segment([egui::pos2(c.x - 1.0, c.y + 2.5), egui::pos2(c.x + 3.5, c.y - 2.5)], egui::Stroke::new(1.5, check_color));
    }
    let text_color = if !enabled {
        egui::Color32::from_rgb(100, 116, 139)
    } else if hovered {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(226, 232, 240)
    };
    let galley = painter.layout_no_wrap(label.into(), egui::FontId::proportional(11.0), text_color);
    painter.galley(
        egui::pos2(box_rect.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    enabled && resp.clicked()
}

fn render_action_btn(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    label: &str,
    id_salt: &str,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(id_salt), egui::Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (bg, stroke_color, text_color) = if hovered {
        (
            egui::Color32::from_rgba_unmultiplied(45, 60, 90, 180),
            egui::Color32::from_rgb(96, 165, 250),
            egui::Color32::WHITE,
        )
    } else {
        (
            egui::Color32::from_rgba_unmultiplied(24, 32, 50, 140),
            egui::Color32::from_rgba_unmultiplied(75, 95, 135, 90),
            egui::Color32::from_rgb(203, 213, 225),
        )
    };
    painter.rect_filled(rect, egui::CornerRadius::same(5), bg);
    painter.rect_stroke(rect, egui::CornerRadius::same(5), egui::Stroke::new(1.0, stroke_color), egui::StrokeKind::Inside);
    let galley = painter.layout_no_wrap(label.into(), egui::FontId::proportional(11.0), text_color);
    painter.galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    resp.clicked()
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
        && pointer.is_some_and(|position| is_live_context_menu_position(rect, position))
    {
        app.context_menu_pos = pointer;
    }
    let Some(menu_position) = app.context_menu_pos else {
        return;
    };

    let scale_label = format!("{:.0} %", app.scale * 100.0);
    let mut reset_requested = false;
    let mut clock_visibility_choice = None;
    let mut second_hand_choice = None;
    let mut repaint_interval_choice = None;
    let mut quota_visibility_choice = None;
    let mut quota_docked_choice = None;
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
                ui.separator();
                let mut show_clock = app.clock_visible;
                if ui
                    .checkbox(
                        &mut show_clock,
                        app.language.text("Analoguhr anzeigen", "Show analog clock"),
                    )
                    .changed()
                {
                    clock_visibility_choice = Some(show_clock);
                }
                let mut show_second_hand = app.clock_second_hand_visible;
                let second_hand_changed = ui
                    .add_enabled_ui(show_clock, |ui| {
                        ui.checkbox(
                            &mut show_second_hand,
                            app.language
                                .text("Orangen Sekundenzeiger anzeigen", "Show orange second hand"),
                        )
                    })
                    .inner
                    .changed();
                if second_hand_changed {
                    second_hand_choice = Some(show_second_hand);
                }
                ui.separator();
                let mut show_quota = app.quota_open;
                if ui
                    .checkbox(
                        &mut show_quota,
                        app.language.text("Limits anzeigen", "Show Limits"),
                    )
                    .changed()
                {
                    quota_visibility_choice = Some(show_quota);
                }
                let mut show_docked = app.quota_docked;
                let docked_changed = ui
                    .add_enabled_ui(show_quota, |ui| {
                        ui.checkbox(
                            &mut show_docked,
                            app.language
                                .text("Limits magnetisch andocken", "Dock Limits magnetically"),
                        )
                    })
                    .inner
                    .changed();
                if docked_changed {
                    quota_docked_choice = Some(show_docked);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        app.language.text("KI-Limits (Setup)", "AI limits (setup)"),
                    )
                    .color(GRAY),
                );
                let mut show_glm = app.quota_show_glm;
                if ui.checkbox(&mut show_glm, "GLM (Z.ai)").changed() {
                    app.quota_show_glm = show_glm;
                    let _ = window_settings::set_live_status_quota_show_glm(show_glm);
                }
                if app.quota_show_glm {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        if let Some(c) = context_menu_swatches(ui, app.quota_color_glm, "ctx_sw_glm", app.language) {
                            app.set_provider_color(ProviderId::Glm, c);
                        }
                    });
                    ui.add_space(2.0);
                }

                let mut show_agi = app.quota_show_agi;
                if ui.checkbox(&mut show_agi, "AGI (Antigravity)").changed() {
                    app.quota_show_agi = show_agi;
                    let _ = window_settings::set_live_status_quota_show_agi(show_agi);
                }
                if app.quota_show_agi {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        if let Some(c) = context_menu_swatches(ui, app.quota_color_agi, "ctx_sw_agi", app.language) {
                            app.set_provider_color(ProviderId::Agy, c);
                        }
                    });
                    ui.add_space(2.0);
                }

                let mut show_codex = app.quota_show_codex;
                if ui.checkbox(&mut show_codex, "Codex (OpenAI)").changed() {
                    app.quota_show_codex = show_codex;
                    let _ = window_settings::set_live_status_quota_show_codex(show_codex);
                }
                if app.quota_show_codex {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        if let Some(c) = context_menu_swatches(ui, app.quota_color_codex, "ctx_sw_codex", app.language) {
                            app.set_provider_color(ProviderId::Codex, c);
                        }
                    });
                    ui.add_space(2.0);
                }

                let mut show_claude = app.quota_show_claude;
                if ui.checkbox(&mut show_claude, "Claude (Anthropic)").changed() {
                    app.quota_show_claude = show_claude;
                    let _ = window_settings::set_live_status_quota_show_claude(show_claude);
                }
                if app.quota_show_claude {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        if let Some(c) = context_menu_swatches(ui, app.quota_color_claude, "ctx_sw_claude", app.language) {
                            app.set_provider_color(ProviderId::Claude, c);
                        }
                    });
                    ui.add_space(2.0);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        app.language.text("Aktualisierungsrate", "Refresh rate"),
                    )
                    .color(GRAY),
                );
                for (interval, text_de, text_en) in REPAINT_INTERVAL_CHOICES {
                    if ui
                        .radio(
                            app.repaint_interval_ms == interval,
                            app.language.text(text_de, text_en),
                        )
                        .clicked()
                    {
                        repaint_interval_choice = Some(interval);
                    }
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        app.language.text("Eckenrundung", "Corner radius"),
                    )
                    .color(GRAY),
                );
                ui.horizontal(|ui| {
                    for &preset in &window_settings::CORNER_RADIUS_PRESETS {
                        let label = format!("{preset}");
                        let is_active = app.corner_radius == preset;
                        let text = egui::RichText::new(label).color(if is_active { egui::Color32::WHITE } else { egui::Color32::from_rgb(148, 163, 184) });
                        if ui.selectable_label(is_active, text).clicked() {
                            app.set_corner_radius(preset);
                        }
                    }
                });
                ui.separator();
                if ui
                    .button(app.language.text(
                        "⚙ Konfigurator & Optionen öffnen...",
                        "⚙ Open configurator & options...",
                    ))
                    .clicked()
                {
                    app.quota_open = true;
                    app.quota_settings_open = true;
                    let _ = window_settings::set_live_status_quota_open(true);
                    let _ = window_settings::set_live_status_quota_settings_open(true);
                }
            });
        });
    let close_after_selection = context_menu_should_close_after_selection(
        reset_requested,
        clock_visibility_choice.is_some(),
        second_hand_choice.is_some(),
        repaint_interval_choice.is_some(),
    ) || quota_visibility_choice.is_some()
        || quota_docked_choice.is_some();
    if let Some(show_clock) = clock_visibility_choice {
        match window_settings::set_clock_visible(show_clock) {
            Ok(()) => app.clock_visible = show_clock,
            Err(error) => app.error = Some(error.to_string()),
        }
    }
    if let Some(show_second_hand) = second_hand_choice {
        match window_settings::set_clock_second_hand_visible(show_second_hand) {
            Ok(()) => app.clock_second_hand_visible = show_second_hand,
            Err(error) => app.error = Some(error.to_string()),
        }
    }
    if let Some(show_quota) = quota_visibility_choice {
        match window_settings::set_live_status_quota_open(show_quota) {
            Ok(()) => app.quota_open = show_quota,
            Err(error) => app.error = Some(error.to_string()),
        }
    }
    if let Some(docked) = quota_docked_choice {
        match window_settings::set_live_status_quota_docked(docked) {
            Ok(()) => app.quota_docked = docked,
            Err(error) => app.error = Some(error.to_string()),
        }
    }
    if let Some(interval) = repaint_interval_choice {
        match window_settings::set_live_status_repaint_interval_ms(interval) {
            Ok(()) => app.repaint_interval_ms = interval,
            Err(error) => app.error = Some(error.to_string()),
        }
    }
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
    } else if close_after_selection
        || (any_click
            && !secondary_clicked
            && pointer.is_some_and(|position| !menu.response.rect.contains(position)))
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
    if app.window_drag_started && primary_down {
        let delta = ui.input(|i| i.pointer.delta());
        if delta.length_sq() > 0.0 {
            if let Some(current_pos) = ui.ctx().input(|i| i.viewport().outer_rect).map(|r| r.min) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                    current_pos + delta,
                ));
            }
        }
    }
    if let Some(position) = position
        && !is_excluded(position)
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
    }
}

struct WindowControls {
    visible: bool,
    hood_rect: egui::Rect,
    log_rect: egui::Rect,
    level_rect: egui::Rect,
    minimize_rect: egui::Rect,
    close_rect: egui::Rect,
    log_hovered: bool,
    level_hovered: bool,
    minimize_hovered: bool,
    close_hovered: bool,
    log_clicked: bool,
    level_clicked: bool,
    minimize_clicked: bool,
    close_clicked: bool,
}

impl WindowControls {
    fn paint(&self, painter: &egui::Painter, level: window_settings::WindowLevel) {
        if !self.visible {
            return;
        }

        // This surface must be fully opaque: the weather line can sit behind
        // it at smaller window scales, but must never shine through controls.
        painter.rect_filled(
            self.hood_rect,
            egui::CornerRadius::same(10),
            WINDOW_CONTROL_HOOD_FILL,
        );
        painter.rect_stroke(
            self.hood_rect,
            egui::CornerRadius::same(10),
            egui::Stroke::new(1.0, WINDOW_CONTROL_HOOD_STROKE),
            egui::StrokeKind::Inside,
        );
        window_chrome::glass_sheen(painter, self.hood_rect);

        let neutral_hover_fill = egui::Color32::from_rgb(52, 54, 66);
        painter.rect_filled(
            self.log_rect,
            egui::CornerRadius::same(8),
            if self.log_hovered {
                neutral_hover_fill
            } else {
                egui::Color32::TRANSPARENT
            },
        );
        painter.rect_filled(
            self.level_rect,
            egui::CornerRadius::same(8),
            if self.level_hovered {
                neutral_hover_fill
            } else {
                egui::Color32::TRANSPARENT
            },
        );
        painter.rect_filled(
            self.minimize_rect,
            egui::CornerRadius::same(8),
            if self.minimize_hovered {
                neutral_hover_fill
            } else {
                egui::Color32::TRANSPARENT
            },
        );
        painter.rect_filled(
            self.close_rect,
            egui::CornerRadius::same(8),
            if self.close_hovered {
                egui::Color32::from_rgb(190, 65, 78)
            } else {
                egui::Color32::TRANSPARENT
            },
        );

        let icon_color = if self.close_hovered {
            egui::Color32::WHITE
        } else {
            GRAY
        };
        draw_log_icon(
            painter,
            self.log_rect.center(),
            if self.log_hovered { ACCENT } else { GRAY },
        );
        draw_window_level_icon(
            painter,
            self.level_rect.center(),
            level,
            if self.level_hovered {
                level_color(level)
            } else {
                GRAY
            },
        );
        painter.line_segment(
            [
                egui::pos2(
                    self.minimize_rect.left() + 11.0,
                    self.minimize_rect.center().y + 4.0,
                ),
                egui::pos2(
                    self.minimize_rect.right() - 11.0,
                    self.minimize_rect.center().y + 4.0,
                ),
            ],
            egui::Stroke::new(1.2, icon_color),
        );
        painter.line_segment(
            [
                egui::pos2(self.close_rect.left() + 11.0, self.close_rect.top() + 10.0),
                egui::pos2(
                    self.close_rect.right() - 11.0,
                    self.close_rect.bottom() - 10.0,
                ),
            ],
            egui::Stroke::new(1.2, icon_color),
        );
        painter.line_segment(
            [
                egui::pos2(self.close_rect.right() - 11.0, self.close_rect.top() + 10.0),
                egui::pos2(
                    self.close_rect.left() + 11.0,
                    self.close_rect.bottom() - 10.0,
                ),
            ],
            egui::Stroke::new(1.2, icon_color),
        );
    }
}

fn window_controls(
    ui: &mut egui::Ui,
    language: Language,
    level: window_settings::WindowLevel,
) -> WindowControls {
    let rect = ui.max_rect();
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
    let mut controls = WindowControls {
        visible,
        hood_rect,
        log_rect,
        level_rect,
        minimize_rect,
        close_rect,
        log_hovered: false,
        level_hovered: false,
        minimize_hovered: false,
        close_hovered: false,
        log_clicked: false,
        level_clicked: false,
        minimize_clicked: false,
        close_clicked: false,
    };
    if !visible {
        return controls;
    }
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
    controls.log_hovered = log_response.hovered();
    controls.level_hovered = level_response.hovered();
    controls.minimize_hovered = minimize_response.hovered();
    controls.close_hovered = close_response.hovered();
    controls.log_clicked = log_response.clicked();
    controls.level_clicked = level_response.clicked();
    controls.minimize_clicked = minimize_response.clicked();
    controls.close_clicked = close_response.clicked();
    controls
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
    weather_symbol: WeatherSymbol,
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
    let weather_symbol = weather_reading
        .map(|reading| reading.symbol)
        .unwrap_or(WeatherSymbol::Unknown);
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
            weather_symbol,
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
            weather_symbol,
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
        weather_symbol,
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

fn pomodoro_minutes_from_vector(offset: egui::Vec2) -> f32 {
    let angle = offset.x.atan2(-offset.y);
    let normalized_angle = if angle < 0.0 { angle + std::f32::consts::TAU } else { angle };
    let turns = normalized_angle / std::f32::consts::TAU;
    (turns * 60.0).round().clamp(1.0, 60.0)
}

fn paint_pomodoro_sector(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    start_turns: f32,
    sweep_turns: f32,
    fill_color: egui::Color32,
    stroke: egui::Stroke,
) {
    if sweep_turns <= f32::EPSILON {
        return;
    }
    let steps = (36.0 * sweep_turns).ceil().clamp(12.0, 48.0) as usize;
    let mut mesh = egui::Mesh::default();
    let center_idx = 0u32;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: center,
        uv: egui::epaint::WHITE_UV,
        color: fill_color,
    });

    let mut arc_points = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = start_turns + sweep_turns * (i as f32 / steps as f32);
        let p = center + clock_direction(t) * radius;
        arc_points.push(p);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: p,
            uv: egui::epaint::WHITE_UV,
            color: fill_color,
        });
        if i > 0 {
            let curr_idx = (i + 1) as u32;
            let prev_idx = curr_idx - 1;
            mesh.indices.extend_from_slice(&[center_idx, prev_idx, curr_idx]);
        }
    }
    painter.add(egui::Shape::mesh(mesh));

    if stroke.width > 0.0 && stroke.color.a() > 0 {
        painter.add(egui::Shape::line(arc_points, stroke));
        painter.line_segment([center, center + clock_direction(start_turns) * radius], stroke);
    }
}

fn paint_pomodoro_center_button(
    painter: &egui::Painter,
    center: egui::Pos2,
    running: bool,
    hovered: bool,
) {
    let btn_radius = 12.5;
    let glow_alpha = if hovered { 75 } else { 35 };
    painter.circle_filled(
        center,
        btn_radius + 3.0,
        egui::Color32::from_rgba_unmultiplied(249, 115, 22, glow_alpha),
    );

    let body_color = if hovered {
        egui::Color32::from_rgb(234, 88, 12)
    } else {
        egui::Color32::from_rgba_unmultiplied(194, 65, 12, 220)
    };
    painter.circle_filled(center, btn_radius, body_color);
    painter.circle_stroke(
        center,
        btn_radius,
        egui::Stroke::new(1.2, egui::Color32::from_rgb(254, 215, 170)),
    );

    if running {
        // Pause ❚❚
        let bar_w = 2.2;
        let bar_h = 7.0;
        let gap = 2.2;
        let left_x = center.x - gap / 2.0 - bar_w;
        let right_x = center.x + gap / 2.0;
        let top_y = center.y - bar_h / 2.0;
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(left_x, top_y), egui::vec2(bar_w, bar_h)),
            egui::CornerRadius::same(1),
            egui::Color32::WHITE,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(right_x, top_y), egui::vec2(bar_w, bar_h)),
            egui::CornerRadius::same(1),
            egui::Color32::WHITE,
        );
    } else {
        // Play ▶
        let tri_w = 7.0;
        let tri_h = 8.0;
        let left = center.x - 2.2;
        let right = left + tri_w;
        let top = center.y - tri_h / 2.0;
        let bottom = center.y + tri_h / 2.0;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(left, top),
                egui::pos2(right, center.y),
                egui::pos2(left, bottom),
            ],
            egui::Color32::WHITE,
            egui::Stroke::NONE,
        ));
    }
}

fn paint_gear_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let r_inner = 3.2;
    let r_outer = 6.2;
    for i in 0..6 {
        let angle = (i as f32) * (std::f32::consts::TAU / 6.0);
        let dir = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [center + dir * r_inner, center + dir * r_outer],
            egui::Stroke::new(2.2, color),
        );
    }
    painter.circle_stroke(center, 4.4, egui::Stroke::new(1.4, color));
    painter.circle_filled(center, 2.0, egui::Color32::from_rgb(11, 17, 32));
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ClockInteraction {
    None,
    TogglePomodoro,
    SetPomodoroSeconds(f32),
    ResetPomodoro,
    TriggerNightAction,
}

#[allow(clippy::too_many_arguments)]
fn moon_icon(
    ui: &mut egui::Ui,
    color: egui::Color32,
    diameter: f32,
    gradient_rect: egui::Rect,
    temperature_c: Option<f64>,
    weather_symbol: WeatherSymbol,
    phase: f64,
    show_clock: bool,
    show_second_hand: bool,
    pomodoro_total_seconds: f32,
    pomodoro_remaining_seconds: f32,
    pomodoro_running: bool,
) -> (egui::Response, ClockInteraction) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click_and_drag());
    let center = rect.center();
    let radius = diameter / 2.0;
    let painter = ui.painter();
    let phase = phase.rem_euclid(1.0);
    let halo_outer = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 18);
    let halo_inner = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32);
    painter.circle_filled(center, radius * MOON_HALO_OUTER_RADIUS, halo_outer);
    painter.circle_filled(center, radius * MOON_HALO_INNER_RADIUS, halo_inner);
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

    let mut interaction = ClockInteraction::None;
    let pointer_pos = ui.input(|i| i.pointer.latest_pos());
    let is_clock_hovered = response.hovered();
    let is_center_hovered = pointer_pos.map_or(false, |p| p.distance(center) <= 14.0);

    if show_clock {
        if response.double_clicked() {
            interaction = ClockInteraction::ResetPomodoro;
        } else if response.secondary_clicked() {
            interaction = ClockInteraction::ResetPomodoro;
        } else if response.dragged() {
            if let Some(pos) = pointer_pos {
                let dist = pos.distance(center);
                if dist > 8.0 {
                    let mins = pomodoro_minutes_from_vector(pos - center);
                    interaction = ClockInteraction::SetPomodoroSeconds(mins * 60.0);
                }
            }
        } else if response.clicked() {
            if is_center_hovered {
                interaction = ClockInteraction::TogglePomodoro;
            } else if pomodoro_total_seconds > 0.0 {
                if let Some(pos) = pointer_pos {
                    let mins = pomodoro_minutes_from_vector(pos - center);
                    interaction = ClockInteraction::SetPomodoroSeconds(mins * 60.0);
                }
            } else {
                interaction = ClockInteraction::TriggerNightAction;
            }
        }

        if is_clock_hovered {
            if is_center_hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
        }

        paint_clock_index_marks(painter, center, radius);
        paint_clock_cardinal_labels(painter, center, radius);

        // Pomodoro Time-Timer Arc (Variante 1)
        if pomodoro_total_seconds > 0.0 {
            // 1. Dunkler leichter Schatten: Der ursprünglich eingestellte Zielwert
            if pomodoro_total_seconds > pomodoro_remaining_seconds {
                let total_turns = (pomodoro_total_seconds / 3600.0).clamp(0.0, 1.0);
                paint_pomodoro_sector(
                    painter,
                    center,
                    radius * 0.98,
                    0.0,
                    total_turns,
                    egui::Color32::from_rgba_unmultiplied(15, 23, 42, 160),
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(148, 163, 184, 110)),
                );
                let mark_pos = center + clock_direction(total_turns) * (radius * 0.98);
                painter.circle_filled(mark_pos, 2.0, egui::Color32::from_rgba_unmultiplied(254, 215, 170, 210));
            }

            // 2. Karmesinroter Restzeit-Sektor
            if pomodoro_remaining_seconds > 0.0 {
                let remaining_turns = (pomodoro_remaining_seconds / 3600.0).clamp(0.0, 1.0);
                let (alpha, rim_alpha) = if pomodoro_remaining_seconds <= 60.0 && pomodoro_running {
                    let pulse = (ui.input(|i| i.time) * 4.0).sin().abs() as f32;
                    ((70.0 + pulse * 45.0) as u8, (200.0 + pulse * 55.0) as u8)
                } else {
                    (80, 230)
                };
                paint_pomodoro_sector(
                    painter,
                    center,
                    radius * 0.98,
                    0.0,
                    remaining_turns,
                    egui::Color32::from_rgba_unmultiplied(239, 68, 68, alpha),
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(239, 68, 68, rim_alpha)),
                );
                painter.line_segment(
                    [center, center + clock_direction(remaining_turns) * (radius * 0.98)],
                    egui::Stroke::new(1.6, egui::Color32::from_rgba_unmultiplied(254, 202, 202, rim_alpha)),
                );
            }
        }

        // Zeiger der Uhr
        paint_clock_hands(
            painter,
            center,
            radius,
            show_second_hand,
            main_hand_color_for_moon(color),
        );

        // Center Play/Pause-Knopf: Nur anzeigen, wenn mit der Maus über das Zentrum gehovert wird
        if is_center_hovered {
            paint_pomodoro_center_button(
                painter,
                center,
                pomodoro_running,
                true,
            );
        }

        if pomodoro_total_seconds > 0.0 {
            let header_rect = paint_clock_pomodoro_header(
                painter,
                center,
                radius,
                pomodoro_remaining_seconds,
                pomodoro_running,
            );
            let header_response = ui.interact(
                header_rect,
                ui.make_persistent_id("pomodoro_header_time_badge"),
                egui::Sense::click(),
            );
            if header_response.double_clicked() {
                interaction = ClockInteraction::ResetPomodoro;
            } else if header_response.clicked() {
                interaction = ClockInteraction::TogglePomodoro;
            }
            if header_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        } else if let Some(temperature_c) = temperature_c {
            paint_clock_temperature(painter, center, radius, temperature_c, weather_symbol);
        }
    } else {
        if response.clicked() {
            interaction = ClockInteraction::TriggerNightAction;
        }
        if is_clock_hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
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
    }
    (response, interaction)
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

#[derive(Clone, Copy, Debug)]
struct ClockHandTurns {
    hour: f32,
    minute: f32,
    second: f32,
}

const HOUR_HAND_LENGTH: f32 = 0.72;
const MINUTE_HAND_LENGTH: f32 = 0.96;
const HOUR_HAND_BASE_WIDTH: f32 = 4.4;
const MINUTE_HAND_BASE_WIDTH: f32 = 3.5;
const MAIN_HAND_COLOR: egui::Color32 = PASTEL_GREEN;
const NIGHT_MODE_MAIN_HAND_COLOR: egui::Color32 = PASTEL_YELLOW;
const MAIN_HAND_INNER_OUTLINE: egui::Color32 = egui::Color32::from_rgb(247, 241, 229);
const MAIN_HAND_OUTER_OUTLINE: egui::Color32 = egui::Color32::from_rgb(5, 8, 15);
const SECOND_HAND_SHAFT_WIDTH: f32 = 0.75;
const SECOND_HAND_COLOR: egui::Color32 = egui::Color32::from_rgb(222, 142, 92);
const CLOCK_CARDINAL_LABEL_RADIUS: f32 = 1.145;
const CLOCK_CARDINAL_FONT_NAME: &str = "russo_one";
const CLOCK_CARDINAL_FONT_SIZE: f32 = 9.3;
const CLOCK_TEMPERATURE_FONT_SIZE: f32 = 11.0;
// The weather row lives in the free space above the dial. The latest live
// review places it about ten screen pixels higher than the clock halo.
const CLOCK_TEMPERATURE_VERTICAL_OFFSET: f32 = 2.0;
const CLOCK_WEATHER_SYMBOL_SIZE: f32 = 32.0;
const WEATHER_SYMBOL_COLOR: egui::Color32 = egui::Color32::from_rgb(229, 201, 137);
const WEATHER_SYMBOL_STROKE_WIDTH: f32 = 1.2;
const RUSSO_ONE_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/RussoOne-Regular.ttf");

#[derive(Clone, Copy, Debug)]
struct ClockIndexMarkSpec {
    turns: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Copy, Debug)]
struct ClockCardinalLabelSpec {
    turns: f32,
    text: &'static str,
}

fn clock_index_mark_specs() -> [ClockIndexMarkSpec; 8] {
    const MINOR_HOURS: [u8; 8] = [1, 2, 4, 5, 7, 8, 10, 11];
    std::array::from_fn(|index| ClockIndexMarkSpec {
        turns: MINOR_HOURS[index] as f32 / 12.0,
        width: 2.0,
        height: 3.5,
    })
}

fn clock_cardinal_label_specs() -> [ClockCardinalLabelSpec; 4] {
    [
        ClockCardinalLabelSpec {
            turns: 0.0,
            text: "12",
        },
        ClockCardinalLabelSpec {
            turns: 0.25,
            text: "3",
        },
        ClockCardinalLabelSpec {
            turns: 0.5,
            text: "6",
        },
        ClockCardinalLabelSpec {
            turns: 0.75,
            text: "9",
        },
    ]
}

fn clock_hand_turns(hour: u16, minute: u16, second: u16, milliseconds: u16) -> ClockHandTurns {
    let second = second as f32 + milliseconds as f32 / 1_000.0;
    let minute = minute as f32 + second / 60.0;
    let hour = (hour % 12) as f32 + minute / 60.0;
    ClockHandTurns {
        hour: hour / 12.0,
        minute: minute / 60.0,
        second: second / 60.0,
    }
}

fn local_clock_hand_turns() -> ClockHandTurns {
    let mut local_time = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut local_time) };
    clock_hand_turns(
        local_time.wHour,
        local_time.wMinute,
        local_time.wSecond,
        local_time.wMilliseconds,
    )
}

fn clock_direction(turns: f32) -> egui::Vec2 {
    let angle = turns * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    egui::vec2(angle.cos(), angle.sin())
}

fn paint_clock_index_marks(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    let color = egui::Color32::from_rgba_unmultiplied(247, 241, 229, 72);
    for mark in clock_index_mark_specs() {
        painter.line_segment(
            clock_index_mark_segment(center, radius, mark.turns, mark.height),
            egui::Stroke::new(mark.width, color),
        );
    }
}

fn paint_clock_cardinal_labels(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    let fill = egui::Color32::from_rgb(225, 232, 190);
    let outline = egui::Color32::from_rgba_unmultiplied(5, 8, 15, 218);
    let font = russo_one_font(CLOCK_CARDINAL_FONT_SIZE);
    for label in clock_cardinal_label_specs() {
        let position = clock_cardinal_label_position(center, radius, label.turns);
        paint_outlined_label(painter, position, label.text, font.clone(), fill, outline);
    }
}

fn russo_one_font(size: f32) -> egui::FontId {
    egui::FontId::new(
        size,
        egui::FontFamily::Name(Arc::from(CLOCK_CARDINAL_FONT_NAME)),
    )
}

fn clock_cardinal_label_position(center: egui::Pos2, radius: f32, turns: f32) -> egui::Pos2 {
    center + clock_direction(turns) * radius * CLOCK_CARDINAL_LABEL_RADIUS
}

fn clock_index_mark_segment(
    center: egui::Pos2,
    radius: f32,
    turns: f32,
    height: f32,
) -> [egui::Pos2; 2] {
    let direction = clock_direction(turns);
    let inner = center + direction * radius * 1.06;
    [inner, inner + direction * height]
}

fn needle_hand_points(
    center: egui::Pos2,
    radius: f32,
    turns: f32,
    length: f32,
    max_width: f32,
) -> [egui::Pos2; 5] {
    let direction = clock_direction(turns);
    let tangent = egui::vec2(-direction.y, direction.x);
    let root_half_width = max_width * 0.24;
    let shoulder = center + direction * radius * length * 0.8;
    let shoulder_half_width = max_width / 2.0;
    [
        center - tangent * root_half_width,
        shoulder - tangent * shoulder_half_width,
        center + direction * radius * length,
        shoulder + tangent * shoulder_half_width,
        center + tangent * root_half_width,
    ]
}

fn second_hand_points(center: egui::Pos2, radius: f32, turns: f32) -> [egui::Pos2; 5] {
    let direction = clock_direction(turns);
    let tangent = egui::vec2(-direction.y, direction.x);
    let half_width = SECOND_HAND_SHAFT_WIDTH / 2.0;
    let tail = center - direction * radius * 0.16;
    let shoulder = center + direction * radius * 0.88;
    [
        tail - tangent * half_width,
        shoulder - tangent * half_width,
        center + direction * radius * 1.02,
        shoulder + tangent * half_width,
        tail + tangent * half_width,
    ]
}

fn paint_layered_needle_hand(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    turns: f32,
    length: f32,
    max_width: f32,
    fill: egui::Color32,
) {
    let points = needle_hand_points(center, radius, turns, length, max_width);
    let one_pixel = outline_width_in_points(painter.pixels_per_point());
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        fill,
        egui::Stroke::new(one_pixel * 2.0, MAIN_HAND_OUTER_OUTLINE),
    ));
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        fill,
        egui::Stroke::new(one_pixel, MAIN_HAND_INNER_OUTLINE),
    ));
}

fn paint_outlined_hand_polygon<const N: usize>(
    painter: &egui::Painter,
    points: [egui::Pos2; N],
    color: egui::Color32,
) {
    let outline = egui::Color32::from_rgba_unmultiplied(5, 8, 15, 220);
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        color,
        egui::Stroke::new(outline_width_in_points(painter.pixels_per_point()), outline),
    ));
}

fn paint_clock_hands(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    show_second_hand: bool,
    main_hand_color: egui::Color32,
) {
    let turns = local_clock_hand_turns();

    paint_layered_needle_hand(
        painter,
        center,
        radius,
        turns.hour,
        HOUR_HAND_LENGTH,
        HOUR_HAND_BASE_WIDTH,
        main_hand_color,
    );
    paint_layered_needle_hand(
        painter,
        center,
        radius,
        turns.minute,
        MINUTE_HAND_LENGTH,
        MINUTE_HAND_BASE_WIDTH,
        main_hand_color,
    );
    if show_second_hand {
        paint_outlined_hand_polygon(
            painter,
            second_hand_points(center, radius, turns.second),
            SECOND_HAND_COLOR,
        );
    }

    let pin_outline = 2.6 + outline_width_in_points(painter.pixels_per_point());
    painter.circle_filled(
        center,
        pin_outline,
        egui::Color32::from_rgba_unmultiplied(5, 8, 15, 220),
    );
    painter.circle_filled(
        center,
        1.7,
        if show_second_hand {
            SECOND_HAND_COLOR
        } else {
            MAIN_HAND_INNER_OUTLINE
        },
    );
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

#[allow(clippy::too_many_arguments)]
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

fn main_hand_color_for_moon(moon_color: egui::Color32) -> egui::Color32 {
    if moon_color == GREEN {
        NIGHT_MODE_MAIN_HAND_COLOR
    } else {
        MAIN_HAND_COLOR
    }
}

fn clock_temperature_position(center: egui::Pos2, radius: f32) -> egui::Pos2 {
    center - egui::vec2(0.0, radius * CLOCK_TEMPERATURE_VERTICAL_OFFSET)
}

fn paint_clock_pomodoro_header(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    remaining_seconds: f32,
    running: bool,
) -> egui::Rect {
    let mins = (remaining_seconds / 60.0).floor() as u32;
    let secs = (remaining_seconds % 60.0).floor() as u32;
    let text = format!("{mins:02}:{secs:02}");
    let font = egui::FontId::proportional(CLOCK_TEMPERATURE_FONT_SIZE);
    let color = if running {
        egui::Color32::from_rgb(252, 165, 165)
    } else {
        egui::Color32::from_rgb(253, 186, 116)
    };
    let text_width = painter
        .layout_no_wrap(text.clone(), font.clone(), color)
        .size()
        .x;
    let position = clock_temperature_position(center, radius);

    let icon_size = 13.0;
    let gap = 5.0;
    let group_width = icon_size + gap + text_width;
    let group_left = position.x - group_width / 2.0;

    let icon_center = egui::pos2(group_left + icon_size / 2.0, position.y);
    paint_pomodoro_header_icon(painter, icon_center, color);

    let text_left = group_left + icon_size + gap;
    painter.text(
        egui::pos2(text_left, position.y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        color,
    );

    egui::Rect::from_center_size(position, egui::vec2(group_width + 8.0, 20.0))
}

fn paint_pomodoro_header_icon(painter: &egui::Painter, center: egui::Pos2, color: egui::Color32) {
    let r = 4.8;
    // Outer dial circle
    painter.circle_stroke(center, r, egui::Stroke::new(1.1, color));
    // Top button/crown
    painter.line_segment(
        [center + egui::vec2(-1.5, -r - 1.4), center + egui::vec2(1.5, -r - 1.4)],
        egui::Stroke::new(1.1, color),
    );
    painter.line_segment(
        [center + egui::vec2(0.0, -r), center + egui::vec2(0.0, -r - 1.4)],
        egui::Stroke::new(1.0, color),
    );
    // Hands: 12 and 2 o'clock
    painter.line_segment(
        [center, center + egui::vec2(0.0, -2.6)],
        egui::Stroke::new(1.1, color),
    );
    painter.line_segment(
        [center, center + egui::vec2(2.0, -1.2)],
        egui::Stroke::new(1.1, color),
    );
}

fn paint_clock_temperature(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    temperature_c: f64,
    weather_symbol: WeatherSymbol,
) {
    let text = moon_temperature_label(temperature_c);
    let font = egui::FontId::proportional(CLOCK_TEMPERATURE_FONT_SIZE);
    let text_color = egui::Color32::from_rgb(202, 214, 208);
    let text_width = painter
        .layout_no_wrap(text.clone(), font.clone(), text_color)
        .size()
        .x;
    let position = clock_temperature_position(center, radius);
    let has_symbol = weather_symbol != WeatherSymbol::Unknown;
    let gap = if has_symbol { 4.0 } else { 0.0 };
    let group_width = if has_symbol {
        CLOCK_WEATHER_SYMBOL_SIZE + gap + text_width
    } else {
        text_width
    };
    let group_left = position.x - group_width / 2.0;
    let text_left = if has_symbol {
        let icon_center = egui::pos2(group_left + CLOCK_WEATHER_SYMBOL_SIZE / 2.0, position.y);
        paint_b_weather_symbol(
            painter,
            icon_center,
            CLOCK_WEATHER_SYMBOL_SIZE,
            weather_symbol,
            WEATHER_SYMBOL_COLOR,
        );
        icon_center.x + CLOCK_WEATHER_SYMBOL_SIZE / 2.0 + gap
    } else {
        group_left
    };
    painter.text(
        egui::pos2(text_left, position.y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        text_color,
    );
}

fn paint_b_weather_symbol(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    symbol: WeatherSymbol,
    color: egui::Color32,
) {
    match symbol {
        WeatherSymbol::ClearDay => paint_b_clear_day(painter, center, size, color),
        WeatherSymbol::ClearNight => paint_b_clear_night(painter, center, size, color),
        WeatherSymbol::PartlyCloudy => paint_b_partly_cloudy(painter, center, size, color),
        WeatherSymbol::Overcast => paint_b_overcast(painter, center, size, color),
        WeatherSymbol::Fog => paint_b_fog(painter, center, size, color),
        WeatherSymbol::Rain => paint_b_rain(painter, center, size, color),
        WeatherSymbol::Snow => paint_b_snow(painter, center, size, color),
        WeatherSymbol::Storm => paint_b_storm(painter, center, size, color),
        WeatherSymbol::Wind => paint_b_wind(painter, center, size, color),
        WeatherSymbol::Unknown => {}
    }
}

fn weather_symbol_position(center: egui::Pos2, size: f32, x: f32, y: f32) -> egui::Pos2 {
    let scale = size / 64.0;
    center + egui::vec2((x - 32.0) * scale, (y - 32.0) * scale)
}

fn weather_symbol_stroke(painter: &egui::Painter, size: f32, color: egui::Color32) -> egui::Stroke {
    let width = (WEATHER_SYMBOL_STROKE_WIDTH * size / 64.0)
        .max(outline_width_in_points(painter.pixels_per_point()));
    egui::Stroke::new(width, color)
}

fn paint_weather_line(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    points: &[(f32, f32)],
    stroke: egui::Stroke,
) {
    painter.add(egui::Shape::line(
        points
            .iter()
            .map(|&(x, y)| weather_symbol_position(center, size, x, y))
            .collect(),
        stroke,
    ));
}

fn paint_weather_curve(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    points: [(f32, f32); 4],
    stroke: egui::Stroke,
) {
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        points.map(|(x, y)| weather_symbol_position(center, size, x, y)),
        false,
        egui::Color32::TRANSPARENT,
        stroke,
    ));
}

fn paint_b_clear_day(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    let scale = size / 64.0;
    painter.circle_stroke(
        weather_symbol_position(center, size, 32.0, 32.0),
        10.3 * scale,
        stroke,
    );
    painter.circle_stroke(
        weather_symbol_position(center, size, 32.0, 32.0),
        8.3 * scale,
        stroke,
    );
    for line in [
        [(32.0, 6.5), (32.0, 17.0)],
        [(32.0, 47.0), (32.0, 57.5)],
        [(6.5, 32.0), (17.0, 32.0)],
        [(47.0, 32.0), (57.5, 32.0)],
        [(13.9, 13.9), (21.3, 21.3)],
        [(42.7, 42.7), (50.1, 50.1)],
        [(50.1, 13.9), (42.7, 21.3)],
        [(21.3, 42.7), (13.9, 50.1)],
    ] {
        paint_weather_line(painter, center, size, &line, stroke);
    }
    painter.circle_stroke(
        weather_symbol_position(center, size, 32.0, 32.0),
        2.2 * scale,
        stroke,
    );
}

fn paint_b_clear_night(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_weather_curve(
        painter,
        center,
        size,
        [(42.8, 10.5), (26.0, 12.0), (19.4, 29.6), (27.1, 41.4)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(27.1, 41.4), (34.4, 52.4), (48.2, 50.8), (55.0, 43.0)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(55.0, 43.0), (42.3, 45.3), (30.3, 32.8), (36.4, 21.8)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(36.4, 21.8), (38.2, 18.2), (40.1, 13.6), (42.8, 10.5)],
        stroke,
    );
    paint_weather_line(painter, center, size, &[(47.5, 16.0), (47.5, 21.3)], stroke);
    paint_weather_line(
        painter,
        center,
        size,
        &[(44.9, 18.65), (50.1, 18.65)],
        stroke,
    );
    paint_weather_line(painter, center, size, &[(16.0, 20.0), (18.8, 20.0)], stroke);
    paint_weather_line(painter, center, size, &[(17.4, 18.6), (17.4, 21.4)], stroke);
}

fn paint_b_cloud(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_weather_curve(
        painter,
        center,
        size,
        [(12.0, 34.0), (13.0, 28.0), (18.0, 25.0), (24.0, 26.0)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(24.0, 26.0), (26.0, 17.0), (39.0, 17.0), (42.0, 25.0)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(42.0, 25.0), (48.0, 21.0), (55.0, 25.0), (54.0, 31.0)],
        stroke,
    );
    paint_weather_curve(
        painter,
        center,
        size,
        [(54.0, 31.0), (54.0, 35.0), (51.0, 36.0), (48.0, 36.0)],
        stroke,
    );
    paint_weather_line(painter, center, size, &[(48.0, 36.0), (12.0, 36.0)], stroke);
}

fn paint_b_partly_cloudy(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let stroke = weather_symbol_stroke(painter, size, color);
    let scale = size / 64.0;
    painter.circle_stroke(
        weather_symbol_position(center, size, 23.0, 23.0),
        8.1 * scale,
        stroke,
    );
    painter.circle_stroke(
        weather_symbol_position(center, size, 23.0, 23.0),
        6.2 * scale,
        stroke,
    );
    for line in [
        [(23.0, 7.5), (23.0, 12.4)],
        [(8.0, 23.0), (12.9, 23.0)],
        [(12.4, 12.4), (15.9, 15.9)],
        [(33.6, 12.4), (30.1, 15.9)],
    ] {
        paint_weather_line(painter, center, size, &line, stroke);
    }
    paint_b_cloud(painter, center, size, color);
}

fn paint_b_overcast(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_b_cloud(painter, center, size, color);
    paint_weather_line(painter, center, size, &[(14.0, 46.0), (51.0, 46.0)], stroke);
    paint_weather_line(painter, center, size, &[(17.0, 49.5), (47.0, 49.5)], stroke);
}

fn paint_b_fog(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_b_cloud(painter, center, size, color);
    for line in [
        [(8.0, 40.0), (49.0, 40.0)],
        [(16.0, 44.5), (57.0, 44.5)],
        [(7.0, 49.0), (43.0, 49.0)],
        [(14.0, 53.5), (49.0, 53.5)],
    ] {
        paint_weather_line(painter, center, size, &line, stroke);
    }
}

fn paint_b_rain(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_b_cloud(painter, center, size, color);
    for line in [
        [(20.0, 42.0), (17.2, 51.5)],
        [(26.5, 42.0), (23.7, 51.5)],
        [(39.0, 42.0), (36.2, 51.5)],
        [(45.5, 42.0), (42.7, 51.5)],
    ] {
        paint_weather_line(painter, center, size, &line, stroke);
    }
}

fn paint_b_snowflake(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    x: f32,
    color: egui::Color32,
) {
    let stroke = weather_symbol_stroke(painter, size, color);
    let y = 47.0;
    for line in [
        [(x, y - 6.0), (x, y + 6.0)],
        [(x - 5.2, y - 3.0), (x + 5.2, y + 3.0)],
        [(x - 5.2, y + 3.0), (x + 5.2, y - 3.0)],
    ] {
        paint_weather_line(painter, center, size, &line, stroke);
    }
}

fn paint_b_snow(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    paint_b_cloud(painter, center, size, color);
    for x in [18.0, 32.0, 46.0] {
        paint_b_snowflake(painter, center, size, x, color);
    }
}

fn paint_b_storm(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    paint_b_cloud(painter, center, size, color);
    paint_weather_line(
        painter,
        center,
        size,
        &[
            (34.0, 38.0),
            (26.0, 49.0),
            (33.0, 49.0),
            (29.0, 57.0),
            (44.0, 43.0),
            (36.0, 43.0),
            (40.0, 38.0),
        ],
        stroke,
    );
    paint_weather_line(painter, center, size, &[(15.0, 38.0), (18.0, 41.0)], stroke);
    paint_weather_line(painter, center, size, &[(49.0, 38.0), (46.0, 41.0)], stroke);
}

fn paint_b_wind(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let stroke = weather_symbol_stroke(painter, size, color);
    for curve in [
        [(8.0, 22.5), (20.0, 22.5), (31.0, 22.5), (36.5, 22.5)],
        [(36.5, 22.5), (43.8, 22.5), (43.8, 13.0), (37.9, 13.0)],
        [(37.9, 13.0), (35.6, 13.0), (34.1, 14.4), (33.3, 16.0)],
        [(8.0, 33.0), (22.0, 33.0), (39.0, 33.0), (49.5, 33.0)],
        [(49.5, 33.0), (57.7, 33.0), (57.8, 43.8), (51.4, 43.8)],
        [(51.4, 43.8), (48.9, 43.8), (47.3, 42.5), (46.5, 40.8)],
        [(16.0, 43.5), (23.0, 43.5), (31.7, 43.5), (37.5, 43.5)],
        [(37.5, 43.5), (43.8, 43.5), (44.0, 52.8), (38.3, 52.8)],
        [(38.3, 52.8), (36.1, 52.8), (34.7, 51.7), (34.0, 50.2)],
    ] {
        paint_weather_curve(painter, center, size, curve, stroke);
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
    media: Option<&MediaSnapshot>,
    metrics_text_right: f32,
    quota_snapshot: &QuotaSnapshot,
    quota_open: bool,
    language: Language,
    show_glm: bool,
    show_agi: bool,
    show_codex: bool,
    show_claude: bool,
) -> (Option<i64>, bool) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let painter = ui.painter();

    // 1. Quota toggle compact button on the far left: [ · ⚡ ]
    let quota_btn_width = 28.0;
    let quota_btn_rect =
        egui::Rect::from_min_size(rect.min, egui::vec2(quota_btn_width, rect.height()));
    let quota_response = ui.interact(
        quota_btn_rect,
        ui.make_persistent_id("quota_toggle_media_button"),
        egui::Sense::click(),
    );
    let quota_hovered = quota_response.hovered();
    if quota_hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let toggle_clicked = quota_response.clicked();

    let btn_bg = if quota_open {
        egui::Color32::from_rgb(26, 38, 62)
    } else if quota_hovered {
        egui::Color32::from_rgb(22, 32, 54)
    } else {
        WINDOW_CONTROL_HOOD_FILL
    };
    let btn_stroke_color = if quota_open {
        egui::Color32::from_rgba_unmultiplied(96, 165, 250, 200)
    } else if quota_hovered {
        egui::Color32::from_rgba_unmultiplied(120, 140, 180, 150)
    } else {
        WINDOW_CONTROL_HOOD_STROKE
    };
    painter.rect_filled(quota_btn_rect, egui::CornerRadius::same(5), btn_bg);
    painter.rect_stroke(
        quota_btn_rect,
        egui::CornerRadius::same(5),
        egui::Stroke::new(1.0, btn_stroke_color),
        egui::StrokeKind::Inside,
    );
    window_chrome::glass_sheen(painter, quota_btn_rect);

    // Micro-LED beacon based only on visible providers:
    let beacon_center = egui::pos2(quota_btn_rect.left() + 8.0, quota_btn_rect.center().y);
    let throttled_visible: Vec<&'static str> = quota_snapshot
        .providers
        .iter()
        .filter(|p| {
            let visible = match p.id {
                ProviderId::Glm => show_glm,
                ProviderId::Agy => show_agi,
                ProviderId::Codex => show_codex,
                ProviderId::Claude => show_claude,
            };
            visible && p.is_throttled
        })
        .map(|p| p.title)
        .collect();

    let has_visible_throttle = !throttled_visible.is_empty();
    let beacon_color = if has_visible_throttle {
        egui::Color32::from_rgb(248, 113, 113)
    } else {
        egui::Color32::from_rgb(74, 222, 128)
    };
    let halo_color = if has_visible_throttle {
        egui::Color32::from_rgba_unmultiplied(248, 113, 113, 60)
    } else {
        egui::Color32::from_rgba_unmultiplied(74, 222, 128, 45)
    };
    painter.circle_filled(beacon_center, 4.0, halo_color);
    painter.circle_filled(beacon_center, 2.2, beacon_color);

    // Bolt icon:
    let bolt_galley = painter.layout_no_wrap(
        "⚡".into(),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(251, 191, 36),
    );
    let bolt_pos = egui::pos2(
        quota_btn_rect.left() + 14.0,
        quota_btn_rect.center().y - bolt_galley.size().y / 2.0,
    );
    painter.galley(bolt_pos, bolt_galley, egui::Color32::from_rgb(251, 191, 36));

    let tooltip = if has_visible_throttle {
        let count = throttled_visible.len();
        let list = throttled_visible.join(", ");
        match language {
            Language::German => format!("⚡ Limits: {count} Anbieter gedrosselt ({list})\nKlicken zum Ein-/Ausblenden"),
            Language::English => format!("⚡ Limits: {count} provider throttled ({list})\nClick to toggle"),
        }
    } else {
        language.text(
            "⚡ Limits: Alle aktiven Anbieter im Rahmen\nKlicken zum Ein-/Ausblenden",
            "⚡ Limits: All active providers within limits\nClick to toggle",
        ).to_string()
    };
    quota_response.on_hover_text(tooltip);

    // 2. Media details or placeholder to the right:
    let media_start_x = quota_btn_rect.right() + 6.0;
    let Some(media) = media else {
        let idle_galley = painter.layout_no_wrap(
            language
                .text("Keine Medienwiedergabe", "No media playback")
                .into(),
            egui::FontId::proportional(9.5),
            egui::Color32::from_rgba_unmultiplied(148, 163, 184, 90),
        );
        let idle_pos = egui::pos2(
            media_start_x + 4.0,
            rect.center().y - idle_galley.size().y / 2.0,
        );
        painter.galley(
            idle_pos,
            idle_galley,
            egui::Color32::from_rgba_unmultiplied(148, 163, 184, 90),
        );
        return (None, toggle_clicked);
    };

    let (artist_color, _, title_color, _) = media_colors(media);
    let artist = if media.artist.trim().is_empty() {
        "Unbekannter Interpret".to_string()
    } else {
        media.artist.clone()
    };
    let timeline_reserve = 110.0;
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(media_start_x, rect.top()),
        egui::pos2(
            (rect.right() - timeline_reserve - 6.0).max(media_start_x),
            rect.bottom(),
        ),
    );
    let max_chars = (text_rect.width() / 6.2).floor().max(14.0) as usize;
    let artist_part = truncate_media_text(&artist, (max_chars as f32 * 0.68) as usize);
    let title_budget = max_chars.saturating_sub(artist_part.chars().count()).max(8);
    let title_part = truncate_media_text(&media.title, title_budget);
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
        ((media.current_position_100ns() - media.start_100ns) as f32 / duration as f32)
            .clamp(0.0, 1.0)
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
    let seek_action = if timeline_response.clicked() && media.seek_enabled && duration > 0 {
        let pointer_x = timeline_response
            .interact_pointer_pos()
            .map(|p| p.x)
            .unwrap_or(track_left);
        let ratio = ((pointer_x - track_left) / track_width).clamp(0.0, 1.0);
        Some(media.start_100ns + (duration as f32 * ratio) as i64)
    } else {
        None
    };

    (seek_action, toggle_clicked)
}

fn render_quota_satellite_window(app: &mut LiveStatusApp, ctx: &egui::Context) {
    let quota_viewport_id = egui::ViewportId::from_hash_of("live_status_quota_viewport");
    let title = match app.language {
        Language::German => "Herdr-Nachtwächter - Limits",
        Language::English => "Herdr Night Watch - Limits",
    };

    let scale = app.scale;
    let quota_width = DESIGN_WIDTH * scale;

    let show_glm = app.quota_show_glm;
    let show_agi = app.quota_show_agi;
    let show_codex = app.quota_show_codex;
    let show_claude = app.quota_show_claude;

    let visible_providers: Vec<ProviderQuota> = app
        .quota_snapshot
        .providers
        .iter()
        .filter(|p| match p.id {
            ProviderId::Glm => show_glm,
            ProviderId::Agy => show_agi,
            ProviderId::Codex => show_codex,
            ProviderId::Claude => show_claude,
        })
        .cloned()
        .collect();

    let sat_design_height = quota_satellite_design_height(&visible_providers, app.quota_settings_open);
    if app.last_synced_sat_height != Some(sat_design_height) {
        app.last_synced_sat_height = Some(sat_design_height);
        app.sync_corner_regions();
        ctx.send_viewport_cmd_to(
            quota_viewport_id,
            egui::ViewportCommand::InnerSize(egui::vec2(DESIGN_WIDTH, sat_design_height)),
        );
    }
    let quota_height = sat_design_height * scale;

    let dock_overlap = 1.0;
    let main_outer_rect = ctx.input(|i| i.viewport().outer_rect);
    let target_pos = if app.quota_docked {
        main_outer_rect.map(|main| egui::pos2(main.min.x, main.max.y - dock_overlap))
    } else {
        app.quota_pos.map(|p| egui::pos2(p[0], p[1])).or_else(|| {
            main_outer_rect.map(|main| egui::pos2(main.min.x, main.max.y + 35.0 * scale))
        })
    };

    let mut builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_taskbar(false)
        .with_inner_size([quota_width, quota_height])
        .with_decorations(false)
        .with_window_level(window_chrome::window_level(app.window_level));

    if let Some(pos) = target_pos {
        if app.satellite_configured_hwnd.is_none() && app.quota_docked && !app.quota_drag_active {
            builder = builder.with_position(pos);
        }
    }

    let is_docked = app.quota_docked;
    let language = app.language;

    let mut user_toggled_dock = false;
    let mut user_toggled_settings = false;
    let mut user_closed_window = false;
    let mut user_drag_started = false;
    let mut user_drag_stopped = false;
    let mut detected_dock_change = None;
    let mut detected_new_pos = None;
    let mut detected_scale_change = None;

    ctx.show_viewport_immediate(quota_viewport_id, builder, |sat_ctx, class| {
        if class != egui::ViewportClass::Immediate {
            return;
        }
        app.satellite_frames_painted = app.satellite_frames_painted.saturating_add(1);
        if app.satellite_frames_painted == 1 {
            sat_ctx.request_repaint();
            app.sync_corner_regions();
        }
        sat_ctx.set_zoom_factor(scale);

        // Auto-reconcile inner size: ensure satellite window matches exact design dimensions
        let expected_size = egui::vec2(DESIGN_WIDTH, sat_design_height);
        if let Some(current_size) = sat_ctx.input(|i| i.viewport().inner_rect.map(|r| r.size())) {
            if (current_size.x - expected_size.x).abs() > 2.0
                || (current_size.y - expected_size.y).abs() > 2.0
            {
                sat_ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(expected_size));
                if let Some(sat_hwnd) = app.satellite_hwnd {
                    window_chrome::sync_satellite_size(
                        sat_hwnd,
                        sat_design_height,
                        app.main_hwnd,
                        app.corner_radius,
                        is_docked,
                    );
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show(sat_ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter();

                // 1:1 Gehäuse-Hintergrund
                window_chrome::default_gradient(painter, rect);

                let r = app.corner_radius;

                // Titelleiste
                let titlebar_height = 26.0;
                let titlebar_rect = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(rect.width(), titlebar_height),
                );
                let tb_corners = if is_docked {
                    egui::CornerRadius::ZERO
                } else {
                    egui::CornerRadius {
                        nw: r.saturating_sub(1),
                        ne: r.saturating_sub(1),
                        sw: 0,
                        se: 0,
                    }
                };
                painter.rect_filled(titlebar_rect, tb_corners, WINDOW_CONTROL_HOOD_FILL);
                painter.line_segment(
                    [titlebar_rect.left_bottom(), titlebar_rect.right_bottom()],
                    egui::Stroke::new(1.0, WINDOW_CONTROL_HOOD_STROKE),
                );

                // Procedural Grip Dots (no missing emoji glyphs)
                let grip_dot_color = egui::Color32::from_rgb(100, 116, 139);
                for col in 0..2 {
                    let dot_x = titlebar_rect.left() + 9.0 + col as f32 * 4.0;
                    for row in 0..3 {
                        let dot_y = titlebar_rect.center().y - 4.5 + row as f32 * 4.5;
                        painter.circle_filled(egui::pos2(dot_x, dot_y), 1.3, grip_dot_color);
                    }
                }

                // Clean title: LIMITS (or OPTIONEN & LIMITS when in config mode)
                let title_text = if app.quota_settings_open {
                    language.text("OPTIONEN & LIMITS", "OPTIONS & LIMITS")
                } else {
                    language.text("LIMITS", "LIMITS")
                };
                let title_galley = painter.layout_no_wrap(
                    title_text.into(),
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(253, 186, 116),
                );
                painter.galley(
                    egui::pos2(
                        titlebar_rect.left() + 22.0,
                        titlebar_rect.center().y - title_galley.size().y / 2.0,
                    ),
                    title_galley.clone(),
                    egui::Color32::from_rgb(220, 225, 235),
                );

                // Controls on the right:
                // Close button:
                let close_size = 18.0;
                let close_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        titlebar_rect.right() - close_size - 5.0,
                        titlebar_rect.center().y - close_size / 2.0,
                    ),
                    egui::vec2(close_size, close_size),
                );
                let close_resp = ui
                    .interact(
                        close_rect,
                        ui.make_persistent_id("sat_close_btn"),
                        egui::Sense::click(),
                    )
                    .on_hover_text(language.text("Schließen", "Close"));
                if close_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    painter.rect_filled(
                        close_rect,
                        egui::CornerRadius::same(4),
                        egui::Color32::from_rgb(190, 65, 78),
                    );
                }
                // Procedural X: two lines, never breaks or shows tofu boxes
                let c = close_rect.center();
                let r = 4.0;
                let x_color = if close_resp.hovered() {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_rgb(203, 213, 225)
                };
                painter.line_segment(
                    [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
                    egui::Stroke::new(1.5, x_color),
                );
                painter.line_segment(
                    [egui::pos2(c.x - r, c.y + r), egui::pos2(c.x + r, c.y - r)],
                    egui::Stroke::new(1.5, x_color),
                );
                if close_resp.clicked() {
                    user_closed_window = true;
                }

                // Action button: compact vector icon button for dock/undock
                let action_size = 20.0;
                let action_btn_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        close_rect.left() - action_size - 4.0,
                        titlebar_rect.center().y - action_size / 2.0,
                    ),
                    egui::vec2(action_size, action_size),
                );
                let action_resp = ui
                    .interact(
                        action_btn_rect,
                        ui.make_persistent_id("sat_action_btn"),
                        egui::Sense::click(),
                    )
                    .on_hover_text(if is_docked {
                        language.text("Abdocken / Lösen", "Detach")
                    } else {
                        language.text("Magnetisch andocken", "Dock")
                    });
                let action_hovered = action_resp.hovered();
                if action_hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    painter.rect_filled(
                        action_btn_rect,
                        egui::CornerRadius::same(4),
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
                    );
                }
                let icon_color = if action_hovered {
                    egui::Color32::WHITE
                } else if is_docked {
                    egui::Color32::from_rgb(56, 189, 248)
                } else {
                    egui::Color32::from_rgb(74, 222, 128)
                };
                let stroke = egui::Stroke::new(1.4, icon_color);
                let ac = action_btn_rect.center();
                if is_docked {
                    // Detach: horizontal bar at top + arrow pointing down away from it
                    painter.line_segment(
                        [egui::pos2(ac.x - 4.5, ac.y - 4.5), egui::pos2(ac.x + 4.5, ac.y - 4.5)],
                        stroke,
                    );
                    painter.line_segment([egui::pos2(ac.x, ac.y - 1.5), egui::pos2(ac.x, ac.y + 4.5)], stroke);
                    painter.line_segment([egui::pos2(ac.x - 2.5, ac.y + 2.0), egui::pos2(ac.x, ac.y + 4.5)], stroke);
                    painter.line_segment([egui::pos2(ac.x + 2.5, ac.y + 2.0), egui::pos2(ac.x, ac.y + 4.5)], stroke);
                } else {
                    // Dock: horizontal bar at top + arrow pointing up into it
                    painter.line_segment(
                        [egui::pos2(ac.x - 4.5, ac.y - 4.5), egui::pos2(ac.x + 4.5, ac.y - 4.5)],
                        stroke,
                    );
                    painter.line_segment([egui::pos2(ac.x, ac.y + 4.5), egui::pos2(ac.x, ac.y - 1.5)], stroke);
                    painter.line_segment([egui::pos2(ac.x - 2.5, ac.y + 1.0), egui::pos2(ac.x, ac.y - 1.5)], stroke);
                    painter.line_segment([egui::pos2(ac.x + 2.5, ac.y + 1.0), egui::pos2(ac.x, ac.y - 1.5)], stroke);
                }
                if action_resp.clicked() {
                    user_toggled_dock = true;
                }

                // Gear button: compact vector icon button for provider settings (left of Detach button)
                let gear_size = 20.0;
                let gear_btn_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        action_btn_rect.left() - gear_size - 4.0,
                        titlebar_rect.center().y - gear_size / 2.0,
                    ),
                    egui::vec2(gear_size, gear_size),
                );
                let gear_resp = ui
                    .interact(
                        gear_btn_rect,
                        ui.make_persistent_id("sat_gear_btn"),
                        egui::Sense::click(),
                    )
                    .on_hover_text(language.text(
                        "Anbieter auswählen / konfigurieren",
                        "Configure AI providers",
                    ));
                let gear_hovered = gear_resp.hovered();
                if gear_hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    painter.rect_filled(
                        gear_btn_rect,
                        egui::CornerRadius::same(4),
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
                    );
                }
                let gear_color = if gear_hovered || app.quota_settings_open {
                    egui::Color32::from_rgb(249, 115, 22)
                } else {
                    egui::Color32::from_rgb(148, 163, 184)
                };
                paint_gear_icon(painter, gear_btn_rect.center(), gear_color);
                if gear_resp.clicked() {
                    user_toggled_settings = true;
                }

                // Center: Circadian Date & Hourly-Rotating Time Pills (Music-Pill Style)
                let (_year, month, day, weekday, hour, minute) = crate::quota::current_local_datetime();
                let weekday_name = match weekday {
                    1 => language.text("Mo", "Mon"),
                    2 => language.text("Di", "Tue"),
                    3 => language.text("Mi", "Wed"),
                    4 => language.text("Do", "Thu"),
                    5 => language.text("Fr", "Fri"),
                    6 => language.text("Sa", "Sat"),
                    _ => language.text("So", "Sun"),
                };
                let date_text = format!("{weekday_name}, {day:02}.{month:02}.");
                let time_text = format!("{hour:02}:{minute:02}");

                // Day-of-week pastel color (Music-Panel aesthetic)
                let weekday_hue = match weekday {
                    1 => 210.0, // Mo: Ice Blue
                    2 => 175.0, // Di: Teal / Cyan
                    3 => 140.0, // Mi: Mint Green
                    4 => 45.0,  // Do: Warm Amber
                    5 => 25.0,  // Fr: Coral
                    6 => 330.0, // Sa: Rose
                    _ => 275.0, // So: Lavender
                };
                let date_color = hsv_color(weekday_hue, 0.34, 0.98);
                let date_bg = hsv_color(weekday_hue + 180.0, 0.60, 0.44);

                // Hourly-rotating pastel color (Circadian cycle: hour * 15° around the wheel)
                let hour_hue = (hour as f32 * 15.0 + 195.0).rem_euclid(360.0);
                let time_color = hsv_color(hour_hue, 0.35, 0.98);
                let time_bg = hsv_color(hour_hue + 180.0, 0.60, 0.44);

                let date_galley = painter.layout_no_wrap(
                    date_text.into(),
                    egui::FontId::monospace(10.0),
                    date_color,
                );
                let time_galley = painter.layout_no_wrap(
                    time_text.into(),
                    egui::FontId::monospace(10.0),
                    time_color,
                );

                let pill_h = 17.0;
                let date_pill_w = date_galley.size().x + 10.0;
                let time_pill_w = time_galley.size().x + 10.0;
                let pill_gap = 5.0;
                let total_pills_w = date_pill_w + pill_gap + time_pill_w;

                let left_edge = titlebar_rect.left() + 22.0 + title_galley.size().x + 8.0;
                let right_edge = gear_btn_rect.left() - 8.0;
                let pills_cx = (left_edge + right_edge) / 2.0;
                let pills_start_x = pills_cx - total_pills_w / 2.0;
                let pill_y = titlebar_rect.center().y - pill_h / 2.0;

                let date_pill_rect = egui::Rect::from_min_size(
                    egui::pos2(pills_start_x, pill_y),
                    egui::vec2(date_pill_w, pill_h),
                );
                let time_pill_rect = egui::Rect::from_min_size(
                    egui::pos2(date_pill_rect.right() + pill_gap, pill_y),
                    egui::vec2(time_pill_w, pill_h),
                );

                draw_media_panel(painter, date_pill_rect, date_color, date_bg, 10, 36, 48);
                painter.galley(
                    egui::pos2(
                        date_pill_rect.center().x - date_galley.size().x / 2.0,
                        date_pill_rect.center().y - date_galley.size().y / 2.0,
                    ),
                    date_galley,
                    date_color,
                );

                draw_media_panel(painter, time_pill_rect, time_color, time_bg, 10, 36, 48);
                painter.galley(
                    egui::pos2(
                        time_pill_rect.center().x - time_galley.size().x / 2.0,
                        time_pill_rect.center().y - time_galley.size().y / 2.0,
                    ),
                    time_galley,
                    time_color,
                );

                // Titlebar Drag handle interaction (no redundant 0px badge):
                let drag_handle_rect = egui::Rect::from_min_max(
                    titlebar_rect.min,
                    egui::pos2(gear_btn_rect.left() - 4.0, titlebar_rect.bottom()),
                );
                let drag_resp = ui.interact(
                    drag_handle_rect,
                    ui.make_persistent_id("sat_titlebar_drag_handle"),
                    egui::Sense::drag(),
                );
                if drag_resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if drag_resp.drag_started() {
                    if is_docked {
                        // Immediately undock on pull away
                        detected_dock_change = Some(false);
                    }
                    user_drag_started = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag_resp.dragged() {
                    let delta = drag_resp.drag_delta();
                    if delta.length_sq() > 0.0 {
                        if let Some(current_pos) = ui.ctx().input(|i| i.viewport().outer_rect).map(|r| r.min) {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                current_pos + delta,
                            ));
                        }
                    }
                }

                let rows_top = titlebar_rect.bottom() + 4.0;
                let row_height = 56.0;
                let row_gap = 5.0;

                if app.quota_settings_open {
                    // Tab Bar directly underneath titlebar
                    let tab_bar_y = titlebar_rect.bottom() + 4.0;
                    let tab_h = 24.0;
                    let tabs = [
                        (ConfigTab::Providers, language.text("⚡ Limits", "⚡ Limits")),
                        (ConfigTab::Window, language.text("⊞ Fenster", "⊞ Window")),
                        (ConfigTab::System, language.text("⚙ System", "⚙ System")),
                    ];
                    let tab_gap = 5.0;
                    let total_tab_w = rect.width() - 16.0;
                    let tab_w = (total_tab_w - 2.0 * tab_gap) / 3.0;

                    for (i, (tab_kind, label)) in tabs.iter().enumerate() {
                        let tab_x = rect.left() + 8.0 + (i as f32) * (tab_w + tab_gap);
                        let tab_rect = egui::Rect::from_min_size(
                            egui::pos2(tab_x, tab_bar_y),
                            egui::vec2(tab_w, tab_h),
                        );
                        let is_active = app.config_tab == *tab_kind;
                        if render_option_pill(
                            ui,
                            painter,
                            tab_rect,
                            label,
                            is_active,
                            &format!("sat_cfg_tab_{i}"),
                        ) {
                            app.config_tab = *tab_kind;
                        }
                    }

                    let content_top = tab_bar_y + tab_h + 6.0;
                    match app.config_tab {
                        ConfigTab::Providers => {
                            let providers_to_config = [
                                (ProviderId::Glm, "GLM", "Z.ai (GLM-4)", app.quota_show_glm),
                                (ProviderId::Agy, "AGY", "Google (Antigravity)", app.quota_show_agi),
                                (ProviderId::Codex, "CODEX", "OpenAI (Codex)", app.quota_show_codex),
                                (ProviderId::Claude, "CLAUDE", "Anthropic (Claude)", app.quota_show_claude),
                            ];

                            for (idx, (pid, short_name, full_name, is_shown)) in providers_to_config.iter().enumerate() {
                                let y = content_top + idx as f32 * (row_height + row_gap);
                                let row_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect.left() + 8.0, y),
                                    egui::vec2(rect.width() - 16.0, row_height),
                                );
                                let badge_color = app.provider_color(*pid);
                                let current_color_idx = app.provider_color_idx(*pid);

                                let top_area = egui::Rect::from_min_size(
                                    row_rect.min,
                                    egui::vec2(row_rect.width(), 30.0),
                                );
                                let top_resp = ui.interact(
                                    top_area,
                                    ui.make_persistent_id(format!("sat_cfg_top_{idx}")),
                                    egui::Sense::click(),
                                );
                                let top_hovered = top_resp.hovered();
                                if top_hovered {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }

                                let bg_color = if top_hovered {
                                    egui::Color32::from_rgba_unmultiplied(30, 41, 65, 180)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140)
                                };
                                painter.rect_filled(row_rect, egui::CornerRadius::same(6), bg_color);
                                let border_color = if *is_shown {
                                    egui::Color32::from_rgba_unmultiplied(badge_color.r(), badge_color.g(), badge_color.b(), 120)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)
                                };
                                painter.rect_stroke(
                                    row_rect,
                                    egui::CornerRadius::same(6),
                                    egui::Stroke::new(1.0, border_color),
                                    egui::StrokeKind::Inside,
                                );

                                // Badge
                                let badge_w = 44.0;
                                let badge_h = 18.0;
                                let badge_rect = egui::Rect::from_min_size(
                                    egui::pos2(row_rect.left() + 10.0, row_rect.top() + 6.0),
                                    egui::vec2(badge_w, badge_h),
                                );
                                let badge_bg = egui::Color32::from_rgba_unmultiplied(badge_color.r(), badge_color.g(), badge_color.b(), 35);
                                painter.rect_filled(badge_rect, egui::CornerRadius::same(4), badge_bg);
                                painter.rect_stroke(badge_rect, egui::CornerRadius::same(4), egui::Stroke::new(1.0, badge_color), egui::StrokeKind::Inside);
                                let badge_galley = painter.layout_no_wrap(
                                    (*short_name).into(),
                                    egui::FontId::proportional(11.0),
                                    badge_color,
                                );
                                painter.galley(
                                    egui::pos2(
                                        badge_rect.center().x - badge_galley.size().x / 2.0,
                                        badge_rect.center().y - badge_galley.size().y / 2.0,
                                    ),
                                    badge_galley,
                                    badge_color,
                                );

                                // Name
                                let name_galley = painter.layout_no_wrap(
                                    (*full_name).into(),
                                    egui::FontId::proportional(12.0),
                                    if *is_shown { TEXT } else { GRAY },
                                );
                                painter.galley(
                                    egui::pos2(badge_rect.right() + 8.0, badge_rect.center().y - name_galley.size().y / 2.0),
                                    name_galley,
                                    if *is_shown { TEXT } else { GRAY },
                                );

                                // Toggle pill
                                let toggle_w = 34.0;
                                let toggle_h = 18.0;
                                let toggle_rect = egui::Rect::from_min_size(
                                    egui::pos2(row_rect.right() - toggle_w - 10.0, row_rect.top() + 6.0),
                                    egui::vec2(toggle_w, toggle_h),
                                );
                                let track_color = if *is_shown {
                                    egui::Color32::from_rgb(22, 163, 74)
                                } else {
                                    egui::Color32::from_rgb(51, 65, 85)
                                };
                                painter.rect_filled(toggle_rect, egui::CornerRadius::same(9), track_color);
                                let knob_r = toggle_h / 2.0 - 2.0;
                                let knob_x = if *is_shown {
                                    toggle_rect.right() - knob_r - 2.0
                                } else {
                                    toggle_rect.left() + knob_r + 2.0
                                };
                                painter.circle_filled(egui::pos2(knob_x, toggle_rect.center().y), knob_r, egui::Color32::WHITE);

                                if top_resp.clicked() {
                                    let new_val = !*is_shown;
                                    match pid {
                                        ProviderId::Glm => {
                                            app.quota_show_glm = new_val;
                                            let _ = window_settings::set_live_status_quota_show_glm(new_val);
                                        }
                                        ProviderId::Agy => {
                                            app.quota_show_agi = new_val;
                                            let _ = window_settings::set_live_status_quota_show_agi(new_val);
                                        }
                                        ProviderId::Codex => {
                                            app.quota_show_codex = new_val;
                                            let _ = window_settings::set_live_status_quota_show_codex(new_val);
                                        }
                                        ProviderId::Claude => {
                                            app.quota_show_claude = new_val;
                                            let _ = window_settings::set_live_status_quota_show_claude(new_val);
                                        }
                                    }
                                }

                                // Swatches row at bottom half
                                let swatches_y = row_rect.top() + 33.0;
                                let label_text = language.text("Farbe:", "Color:");
                                let label_galley = painter.layout_no_wrap(label_text.into(), egui::FontId::proportional(10.0), GRAY);
                                painter.galley(
                                    egui::pos2(row_rect.left() + 10.0, swatches_y + 1.0),
                                    label_galley,
                                    GRAY,
                                );
                                let swatches_origin = egui::pos2(row_rect.left() + 52.0, swatches_y);
                                if let Some(new_color) = render_palette_swatches(
                                    painter,
                                    ui,
                                    swatches_origin,
                                    current_color_idx,
                                    &format!("sat_cfg_sw_{idx}"),
                                    language,
                                ) {
                                    app.set_provider_color(*pid, new_color);
                                }
                            }
                        }
                        ConfigTab::Window => {
                            let mut wy = content_top;
                            let card_w = rect.width() - 16.0;

                            // 1. Eckenrundung Card (Corner radius)
                            let r_card_h = 56.0;
                            let r_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, wy),
                                egui::vec2(card_w, r_card_h),
                            );
                            painter.rect_filled(r_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(r_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let r_label = language.text("Eckenrundung:", "Corner radius:");
                            let r_galley = painter.layout_no_wrap(r_label.into(), egui::FontId::proportional(11.0), TEXT);
                            painter.galley(egui::pos2(r_card_rect.left() + 10.0, r_card_rect.top() + 7.0), r_galley, TEXT);

                            let val_text = if app.corner_radius == 0 {
                                language.text("0 px (Eckig)", "0 px (Square)")
                            } else if app.corner_radius == window_settings::DEFAULT_LIVE_STATUS_CORNER_RADIUS {
                                language.text("10 px (Standard)", "10 px (Default)")
                            } else {
                                &format!("{} px", app.corner_radius)
                            };
                            let val_galley = painter.layout_no_wrap(val_text.into(), egui::FontId::proportional(10.5), egui::Color32::from_rgb(56, 189, 248));
                            painter.galley(egui::pos2(r_card_rect.right() - val_galley.size().x - 10.0, r_card_rect.top() + 7.0), val_galley, egui::Color32::from_rgb(56, 189, 248));

                            let preset_count = window_settings::CORNER_RADIUS_PRESETS.len();
                            let p_gap = 4.0;
                            let p_w = (card_w - 20.0 - (preset_count - 1) as f32 * p_gap) / (preset_count as f32);
                            let p_h = 22.0;
                            let p_y = r_card_rect.top() + 27.0;

                            for (pi, &preset) in window_settings::CORNER_RADIUS_PRESETS.iter().enumerate() {
                                let px = r_card_rect.left() + 10.0 + (pi as f32) * (p_w + p_gap);
                                let p_rect = egui::Rect::from_min_size(egui::pos2(px, p_y), egui::vec2(p_w, p_h));
                                let p_label = format!("{preset} px");
                                if render_option_pill(ui, painter, p_rect, &p_label, app.corner_radius == preset, &format!("sat_cr_p_{pi}")) {
                                    app.set_corner_radius(preset);
                                }
                            }
                            wy += r_card_h + 5.0;

                            // 2. Fenstertransparenz Card (Window transparency)
                            let op_card_h = 56.0;
                            let op_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, wy),
                                egui::vec2(card_w, op_card_h),
                            );
                            painter.rect_filled(op_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(op_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let op_label = language.text("Fenstertransparenz:", "Window transparency:");
                            let op_galley = painter.layout_no_wrap(op_label.into(), egui::FontId::proportional(11.0), TEXT);
                            painter.galley(egui::pos2(op_card_rect.left() + 10.0, op_card_rect.top() + 7.0), op_galley, TEXT);

                            let cur_trans = app.transparency();
                            let cur_trans_text = if cur_trans == 0 {
                                language.text("0 % (voll sichtbar)", "0% (solid)")
                            } else if cur_trans == 90 {
                                language.text("90 % (fast unsichtbar)", "90% (nearly invisible)")
                            } else {
                                &format!("{cur_trans} %")
                            };
                            let cur_trans_galley = painter.layout_no_wrap(cur_trans_text.into(), egui::FontId::proportional(10.5), egui::Color32::from_rgb(56, 189, 248));
                            painter.galley(egui::pos2(op_card_rect.right() - cur_trans_galley.size().x - 10.0, op_card_rect.top() + 7.0), cur_trans_galley, egui::Color32::from_rgb(56, 189, 248));

                            let trans_presets = window_settings::TRANSPARENCY_PRESETS;
                            let p_count = trans_presets.len();
                            let op_gap = 4.0;
                            let op_w = (card_w - 20.0 - (p_count - 1) as f32 * op_gap) / (p_count as f32);
                            let op_h = 22.0;
                            let op_btn_y = op_card_rect.top() + 27.0;
                            for (oi, &trans_val) in trans_presets.iter().enumerate() {
                                let opx = op_card_rect.left() + 10.0 + (oi as f32) * (op_w + op_gap);
                                let op_rect = egui::Rect::from_min_size(egui::pos2(opx, op_btn_y), egui::vec2(op_w, op_h));
                                let op_btn_label = format!("{trans_val} %");
                                if render_option_pill(ui, painter, op_rect, &op_btn_label, cur_trans == trans_val, &format!("sat_trans_{oi}")) {
                                    app.apply_transparency(trans_val);
                                }
                            }
                            wy += op_card_h + 5.0;

                            // 3. Fensterebene Card (Window level)
                            let lvl_card_h = 50.0;
                            let lvl_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, wy),
                                egui::vec2(card_w, lvl_card_h),
                            );
                            painter.rect_filled(lvl_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(lvl_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let lvl_label = language.text("Fensterebene:", "Window level:");
                            let lvl_galley = painter.layout_no_wrap(lvl_label.into(), egui::FontId::proportional(11.0), TEXT);
                            painter.galley(egui::pos2(lvl_card_rect.left() + 10.0, lvl_card_rect.top() + 6.0), lvl_galley, TEXT);

                            let levels = [
                                (window_settings::WindowLevel::Normal, language.text("Normal", "Normal")),
                                (window_settings::WindowLevel::AlwaysOnTop, language.text("Vordergrund", "On top")),
                                (window_settings::WindowLevel::AlwaysOnBottom, language.text("Hintergrund", "In bg")),
                            ];
                            let lvl_gap = 5.0;
                            let lvl_w = (card_w - 20.0 - 2.0 * lvl_gap) / 3.0;
                            let lvl_h = 20.0;
                            let lvl_btn_y = lvl_card_rect.top() + 24.0;
                            for (li, (target_lvl, lvl_btn_label)) in levels.iter().enumerate() {
                                let lx = lvl_card_rect.left() + 10.0 + (li as f32) * (lvl_w + lvl_gap);
                                let l_rect = egui::Rect::from_min_size(egui::pos2(lx, lvl_btn_y), egui::vec2(lvl_w, lvl_h));
                                if render_option_pill(ui, painter, l_rect, lvl_btn_label, app.window_level == *target_lvl, &format!("sat_lvl_{li}")) {
                                    app.apply_window_level(ui.ctx(), *target_lvl);
                                }
                            }
                            wy += lvl_card_h + 5.0;

                            // 4. Verhalten Card (Taskbar & Docking checkboxes)
                            let beh_card_h = 54.0;
                            let beh_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, wy),
                                egui::vec2(card_w, beh_card_h),
                            );
                            painter.rect_filled(beh_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(beh_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let cur_tb = app.taskbar_visible.unwrap_or_else(window_settings::live_status_in_taskbar);
                            let r1_rect = egui::Rect::from_min_size(
                                egui::pos2(beh_card_rect.left() + 6.0, beh_card_rect.top() + 4.0),
                                egui::vec2(card_w - 12.0, 22.0),
                            );
                            if render_toggle_row(ui, painter, r1_rect, language.text("Live-Fenster in Taskleiste anzeigen", "Show live window in taskbar"), cur_tb, true, "sat_tgl_tb") {
                                let new_tb = !cur_tb;
                                let _ = window_settings::set_live_status_in_taskbar(new_tb);
                                app.taskbar_visible = Some(new_tb);
                                let main_hwnd = app.main_hwnd.or_else(|| find_live_window_for_pid(current_pid()));
                                if let Some(hwnd) = main_hwnd {
                                    app.main_hwnd = Some(hwnd);
                                    let _ = taskbar::set_visible(hwnd, new_tb);
                                }
                                let sat_hwnd = app.satellite_hwnd.or_else(|| find_satellite_window_for_pid(current_pid()));
                                if let Some(sat) = sat_hwnd {
                                    app.satellite_hwnd = Some(sat);
                                    taskbar::set_satellite_exempt(sat, main_hwnd);
                                }
                            }

                            let r2_rect = egui::Rect::from_min_size(
                                egui::pos2(beh_card_rect.left() + 6.0, beh_card_rect.top() + 28.0),
                                egui::vec2(card_w - 12.0, 22.0),
                            );
                            if render_toggle_row(ui, painter, r2_rect, language.text("Limits magnetisch andocken (ohne Nahtrundung)", "Dock Limits magnetically (flat seam)"), is_docked, true, "sat_tgl_dock") {
                                user_toggled_dock = true;
                            }
                        }
                        ConfigTab::System => {
                            let mut sy = content_top;
                            let card_w = rect.width() - 16.0;

                            // 1. Start & Uhr Card
                            let start_card_h = 84.0;
                            let start_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, sy),
                                egui::vec2(card_w, start_card_h),
                            );
                            painter.rect_filled(start_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(start_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let is_autostart = autostart::enabled();
                            let s1_rect = egui::Rect::from_min_size(
                                egui::pos2(start_card_rect.left() + 6.0, start_card_rect.top() + 4.0),
                                egui::vec2(card_w - 12.0, 22.0),
                            );
                            if render_toggle_row(ui, painter, s1_rect, language.text("Mit Windows starten (Autostart)", "Start with Windows (autostart)"), is_autostart, true, "sat_tgl_auto") {
                                let _ = autostart::set_enabled(!is_autostart);
                            }

                            let is_live_start = window_settings::live_status_on_start();
                            let s2_rect = egui::Rect::from_min_size(
                                egui::pos2(start_card_rect.left() + 6.0, start_card_rect.top() + 29.0),
                                egui::vec2(card_w - 12.0, 22.0),
                            );
                            if render_toggle_row(ui, painter, s2_rect, language.text("Live-Fenster beim Start öffnen", "Open live window at startup"), is_live_start, true, "sat_tgl_ls") {
                                let _ = window_settings::set_live_status_on_start(!is_live_start);
                            }

                            // Uhr & Sekundenzeiger nebeneinander
                            let half_w = (card_w - 16.0) / 2.0;
                            let s3_left_rect = egui::Rect::from_min_size(
                                egui::pos2(start_card_rect.left() + 6.0, start_card_rect.top() + 54.0),
                                egui::vec2(half_w, 22.0),
                            );
                            if render_toggle_row(ui, painter, s3_left_rect, language.text("Analoguhr", "Analog clock"), app.clock_visible, true, "sat_tgl_clock") {
                                let new_c = !app.clock_visible;
                                let _ = window_settings::set_clock_visible(new_c);
                                app.clock_visible = new_c;
                            }

                            let s3_right_rect = egui::Rect::from_min_size(
                                egui::pos2(start_card_rect.left() + 10.0 + half_w, start_card_rect.top() + 54.0),
                                egui::vec2(half_w, 22.0),
                            );
                            if render_toggle_row(ui, painter, s3_right_rect, language.text("Sekundenzeiger", "Second hand"), app.clock_second_hand_visible, app.clock_visible, "sat_tgl_sec") {
                                let new_sec = !app.clock_second_hand_visible;
                                let _ = window_settings::set_clock_second_hand_visible(new_sec);
                                app.clock_second_hand_visible = new_sec;
                            }
                            sy += start_card_h + 5.0;

                            // 2. Aktualisierungsrate Card
                            let rate_card_h = 44.0;
                            let rate_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, sy),
                                egui::vec2(card_w, rate_card_h),
                            );
                            painter.rect_filled(rate_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(rate_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let rate_label = language.text("Aktualisierungsrate:", "Refresh rate:");
                            let rate_galley = painter.layout_no_wrap(rate_label.into(), egui::FontId::proportional(11.0), TEXT);
                            painter.galley(egui::pos2(rate_card_rect.left() + 10.0, rate_card_rect.center().y - rate_galley.size().y / 2.0), rate_galley, TEXT);

                            let rate_choices = [(250u32, "250 ms"), (500, "500 ms"), (1000, "1000 ms")];
                            let rate_gap = 5.0;
                            let rate_w = 54.0;
                            let rate_h = 20.0;
                            for (ri, (interval, r_label)) in rate_choices.iter().enumerate() {
                                let rx = rate_card_rect.right() - 10.0 - ((3 - ri) as f32) * (rate_w + rate_gap) + rate_gap;
                                let r_rect = egui::Rect::from_min_size(
                                    egui::pos2(rx, rate_card_rect.center().y - rate_h / 2.0),
                                    egui::vec2(rate_w, rate_h),
                                );
                                if render_option_pill(ui, painter, r_rect, r_label, app.repaint_interval_ms == *interval, &format!("sat_rate_{ri}")) {
                                    let _ = window_settings::set_live_status_repaint_interval_ms(*interval);
                                    app.repaint_interval_ms = *interval;
                                }
                            }
                            sy += rate_card_h + 5.0;

                            // 3. Sprache Card
                            let lang_card_h = 44.0;
                            let lang_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, sy),
                                egui::vec2(card_w, lang_card_h),
                            );
                            painter.rect_filled(lang_card_rect, egui::CornerRadius::same(6), egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140));
                            painter.rect_stroke(lang_card_rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 80)), egui::StrokeKind::Inside);

                            let lang_label = language.text("Sprache / Language:", "Language / Sprache:");
                            let lang_galley = painter.layout_no_wrap(lang_label.into(), egui::FontId::proportional(11.0), TEXT);
                            painter.galley(egui::pos2(lang_card_rect.left() + 10.0, lang_card_rect.center().y - lang_galley.size().y / 2.0), lang_galley, TEXT);

                            let lang_choices = [(Language::German, "Deutsch"), (Language::English, "English")];
                            let lang_gap = 5.0;
                            let lang_w = 64.0;
                            let lang_h = 20.0;
                            for (li, (target_lang, l_label)) in lang_choices.iter().enumerate() {
                                let lx = lang_card_rect.right() - 10.0 - ((2 - li) as f32) * (lang_w + lang_gap) + lang_gap;
                                let l_rect = egui::Rect::from_min_size(
                                    egui::pos2(lx, lang_card_rect.center().y - lang_h / 2.0),
                                    egui::vec2(lang_w, lang_h),
                                );
                                if render_option_pill(ui, painter, l_rect, l_label, app.language == *target_lang, &format!("sat_lang_{li}")) {
                                    let _ = target_lang.set();
                                    app.language = *target_lang;
                                    ui.ctx().send_viewport_cmd_to(
                                        egui::ViewportId::ROOT,
                                        egui::ViewportCommand::Title(live_title(*target_lang).into()),
                                    );
                                    ui.ctx().send_viewport_cmd_to(
                                        egui::ViewportId::from_hash_of("live_status_quota_viewport"),
                                        egui::ViewportCommand::Title(match target_lang {
                                            Language::German => "Herdr-Nachtwächter - Limits",
                                            Language::English => "Herdr Night Watch - Limits",
                                        }.into()),
                                    );
                                }
                            }
                            sy += lang_card_h + 5.0;

                            // 4. Dialog Buttons Card
                            let dlg_card_h = 42.0;
                            let dlg_card_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 8.0, sy),
                                egui::vec2(card_w, dlg_card_h),
                            );
                            let dlg_gap = 5.0;
                            let dlg_w = (card_w - 2.0 * dlg_gap) / 3.0;
                            let dlg_h = 32.0;
                            let dlg_y = dlg_card_rect.center().y - dlg_h / 2.0;

                            let b1_rect = egui::Rect::from_min_size(egui::pos2(dlg_card_rect.left(), dlg_y), egui::vec2(dlg_w, dlg_h));
                            if render_action_btn(ui, painter, b1_rect, language.text("📍 Wetterort", "📍 Location"), "sat_act_weather") {
                                let _ = weather_location::open();
                            }

                            let b2_rect = egui::Rect::from_min_size(egui::pos2(dlg_card_rect.left() + dlg_w + dlg_gap, dlg_y), egui::vec2(dlg_w, dlg_h));
                            if render_action_btn(ui, painter, b2_rect, language.text("📋 Protokoll", "📋 Log"), "sat_act_log") {
                                let _ = log_viewer::open();
                            }

                            let b3_rect = egui::Rect::from_min_size(egui::pos2(dlg_card_rect.left() + 2.0 * (dlg_w + dlg_gap), dlg_y), egui::vec2(dlg_w, dlg_h));
                            if render_action_btn(ui, painter, b3_rect, language.text("⚙ Einrichtung", "⚙ Setup"), "sat_act_setup") {
                                let _ = settings::open();
                            }
                        }
                    }
                } else if visible_providers.is_empty() {
                    let row_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 8.0, rows_top),
                        egui::vec2(rect.width() - 16.0, row_height),
                    );
                    painter.rect_filled(
                        row_rect,
                        egui::CornerRadius::same(6),
                        egui::Color32::from_rgba_unmultiplied(20, 28, 45, 140),
                    );
                    painter.rect_stroke(
                        row_rect,
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 90)),
                        egui::StrokeKind::Inside,
                    );
                    let empty_msg = language.text(
                        "Keine KI-Limits gewählt (Zahnrad ⚙ oben anklicken)",
                        "No AI limits selected (Click gear ⚙ icon above)",
                    );
                    let empty_galley = painter.layout_no_wrap(
                        empty_msg.into(),
                        egui::FontId::proportional(11.5),
                        GRAY,
                    );
                    painter.galley(
                        egui::pos2(
                            row_rect.center().x - empty_galley.size().x / 2.0,
                            row_rect.center().y - empty_galley.size().y / 2.0,
                        ),
                        empty_galley,
                        GRAY,
                    );
                } else {
                    let mut current_y = rows_top;
                    for quota in &visible_providers {
                        let card_h = provider_card_height(quota);
                        let row_rect = egui::Rect::from_min_size(
                            egui::pos2(rect.left() + 8.0, current_y),
                            egui::vec2(rect.width() - 16.0, card_h),
                        );
                        let forecast = quota.pacing_forecast();
                        draw_provider_card(painter, row_rect, quota, &forecast, language, app.provider_color(quota.id));
                        current_y += card_h + 6.0;
                    }
                }

                // Interactive proportional resize grip in the bottom-right corner:
                let sat_grip = resize_grip_rect(rect);
                let sat_grip_resp = ui
                    .interact(
                        sat_grip,
                        ui.make_persistent_id("sat_window_resize_grip"),
                        egui::Sense::drag(),
                    )
                    .on_hover_text(language.text("Größe anpassen", "Resize"));
                if sat_grip_resp.drag_started() {
                    let pointer = ui
                        .ctx()
                        .input(|input| input.pointer.interact_pos())
                        .unwrap_or(sat_grip.right_bottom());
                    let initial_window_size = ui
                        .ctx()
                        .input(|input| input.viewport().inner_rect.map(|rect| rect.size()))
                        .unwrap_or_else(|| rect.size());
                    app.quota_resize_drag = Some((
                        physical_pointer(ui.ctx(), pointer),
                        app.scale,
                        initial_window_size,
                    ));
                    app.resize_preview_scale = Some(app.scale);
                    app.resize_preview_window_size = None;
                }
                if let Some((start_pointer, initial_scale, initial_window_size)) = app.quota_resize_drag {
                    let primary_down = ui.ctx().input(|input| input.pointer.primary_down());
                    if primary_down
                        && let Some(pointer) = ui.ctx().input(|input| input.pointer.interact_pos())
                    {
                        let pointer = physical_pointer(ui.ctx(), pointer);
                        let delta = pointer.x - start_pointer.x;
                        let native_pixels_per_point =
                            ui.ctx().native_pixels_per_point().unwrap_or(1.0);
                        let scale_delta = delta / (DESIGN_WIDTH * native_pixels_per_point);
                        let requested_scale = initial_scale + scale_delta;
                        let preview_scale =
                            window_settings::clamp_live_status_scale(requested_scale);
                        app.resize_preview_scale = Some(preview_scale);

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
                            let main_native_preview_size = egui::vec2(
                                native_preview_size.x,
                                DESIGN_HEIGHT * (native_preview_size.x / DESIGN_WIDTH),
                            );
                            ui.ctx().send_viewport_cmd_to(
                                egui::ViewportId::ROOT,
                                egui::ViewportCommand::InnerSize(main_native_preview_size),
                            );
                            app.resize_preview_window_size = Some(native_preview_size);
                        }
                    }
                    if sat_grip_resp.drag_stopped() || !primary_down {
                        if let Some(preview_scale) = app.resize_preview_scale {
                            detected_scale_change = Some(preview_scale);
                        }
                        app.quota_resize_drag = None;
                        app.resize_preview_scale = None;
                        app.resize_preview_window_size = None;
                        app.window_drag_started = true;
                    }
                }
                if sat_grip_resp.hovered() || sat_grip_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                }
                let grip_color = if sat_grip_resp.hovered() || sat_grip_resp.dragged() {
                    egui::Color32::from_rgba_unmultiplied(226, 232, 240, 180)
                } else {
                    egui::Color32::from_rgba_unmultiplied(148, 163, 184, 80)
                };
                for offset in [0.0, 4.0, 8.0] {
                    painter.line_segment(
                        [
                            egui::pos2(sat_grip.right() - 7.0 - offset, sat_grip.bottom() - 4.0),
                            egui::pos2(sat_grip.right() - 4.0, sat_grip.bottom() - 7.0 - offset),
                        ],
                        egui::Stroke::new(1.0, grip_color),
                    );
                }
                if let Some(preview_scale) = app.resize_preview_scale {
                    let preview_rect = resize_preview_target_rect(rect, app.scale, preview_scale);
                    let preview_color = egui::Color32::from_rgba_unmultiplied(147, 197, 253, 230);
                    let preview_fill = egui::Color32::from_rgba_unmultiplied(15, 31, 56, 128);

                    let r = app.corner_radius;
                    let (corner_radius, inner_corner_radius) = if is_docked {
                        (
                            egui::CornerRadius {
                                nw: 0,
                                ne: 0,
                                sw: r,
                                se: r,
                            },
                            egui::CornerRadius {
                                nw: 0,
                                ne: 0,
                                sw: r.saturating_sub(2),
                                se: r.saturating_sub(2),
                            },
                        )
                    } else {
                        (egui::CornerRadius::same(r), egui::CornerRadius::same(r.saturating_sub(2)))
                    };

                    painter.rect_filled(preview_rect, corner_radius, preview_fill);
                    painter.rect_stroke(
                        preview_rect,
                        corner_radius,
                        egui::Stroke::new(1.0, preview_color),
                        egui::StrokeKind::Inside,
                    );
                    painter.rect_stroke(
                        preview_rect.shrink(4.0),
                        inner_corner_radius,
                        egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(226, 232, 240, 65),
                        ),
                        egui::StrokeKind::Inside,
                    );

                    let target_width = (DESIGN_WIDTH * preview_scale).round() as u32;
                    let target_height = (176.0 * preview_scale).round() as u32;
                    let label = format!(
                        "{:.0} % · {} × {} px",
                        preview_scale * 100.0,
                        target_width,
                        target_height
                    );
                    let galley = painter.layout_no_wrap(label, egui::FontId::proportional(10.0), TEXT);
                    let label_rect = resize_preview_label_rect(rect, preview_rect, galley.size());
                    painter.rect_filled(
                        label_rect,
                        egui::CornerRadius::same(5),
                        egui::Color32::from_rgba_unmultiplied(15, 23, 42, 225),
                    );
                    painter.rect_stroke(
                        label_rect,
                        egui::CornerRadius::same(5),
                        egui::Stroke::new(1.0, preview_color),
                        egui::StrokeKind::Inside,
                    );
                    painter.galley(label_rect.center() - galley.size() / 2.0, galley, TEXT);
                }

                // Handle magnetic snapping and position tracking:
                let sat_outer = ui.ctx().input(|i| i.viewport().outer_rect);
                let primary_down = ui.ctx().input(|i| i.pointer.primary_down());
                if is_docked && !app.quota_drag_active {
                    if let Some(sat) = sat_outer
                        && let Some(main) = main_outer_rect
                    {
                        let dock_target = egui::pos2(main.min.x, main.max.y - dock_overlap);
                        let delta_x = (sat.min.x - dock_target.x).abs();
                        let delta_y = (sat.min.y - dock_target.y).abs();
                        if delta_x > 0.5 || delta_y > 0.5 {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                dock_target,
                            ));
                        }
                    }
                }
                if !primary_down {
                    if app.quota_drag_active {
                        user_drag_stopped = true;
                    }
                    if let Some(sat) = sat_outer
                        && let Some(main) = main_outer_rect
                    {
                        let dock_target = egui::pos2(main.min.x, main.max.y - dock_overlap);
                        let delta_x = (sat.min.x - dock_target.x).abs();
                        let delta_y = (sat.min.y - dock_target.y).abs();
                        if is_docked {
                            if delta_x > 1.0 || delta_y > 1.0 {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                    dock_target,
                                ));
                            }
                        } else if delta_x <= 35.0 && delta_y <= 35.0 {
                            // User dragged it close: Magnetic Snap!
                            detected_dock_change = Some(true);
                            detected_new_pos = Some([dock_target.x, dock_target.y]);
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                dock_target,
                            ));
                        } else {
                            detected_new_pos = Some([sat.min.x, sat.min.y]);
                        }
                    }
                }
            });
    });

    // Apply state changes from the viewport:
    if user_closed_window {
        app.quota_open = false;
        let _ = window_settings::set_live_status_quota_open(false);
    }
    if user_drag_started {
        app.quota_drag_active = true;
    }
    if user_drag_stopped {
        app.quota_drag_active = false;
    }
    if user_toggled_settings {
        app.quota_settings_open = !app.quota_settings_open;
        let _ = window_settings::set_live_status_quota_settings_open(app.quota_settings_open);
        let new_sat_h = quota_satellite_design_height(&visible_providers, app.quota_settings_open);
        app.last_synced_sat_height = Some(new_sat_h);
        if let Some(sat_hwnd) = app.satellite_hwnd {
            window_chrome::sync_satellite_size(
                sat_hwnd,
                new_sat_h,
                app.main_hwnd,
                app.corner_radius,
                app.quota_docked,
            );
        }
        ctx.send_viewport_cmd_to(
            quota_viewport_id,
            egui::ViewportCommand::InnerSize(egui::vec2(DESIGN_WIDTH, new_sat_h)),
        );
        ctx.request_repaint();
    }
    if user_toggled_dock {
        app.quota_docked = !app.quota_docked;
        let _ = window_settings::set_live_status_quota_docked(app.quota_docked);
        if !app.quota_docked {
            // Abschieben: move down by 35px
            if let Some(main) = main_outer_rect {
                let detached_pos = [main.min.x, main.max.y + 35.0 * scale];
                app.quota_pos = Some(detached_pos);
                let _ = window_settings::set_live_status_quota_position(detached_pos);
                ctx.send_viewport_cmd_to(
                    quota_viewport_id,
                    egui::ViewportCommand::OuterPosition(egui::pos2(
                        detached_pos[0],
                        detached_pos[1],
                    )),
                );
            }
        } else if let Some(main) = main_outer_rect {
            // Andocken: snap back to seamless 0px gap
            let docked_pos = [main.min.x, main.max.y - dock_overlap];
            app.quota_pos = Some(docked_pos);
            let _ = window_settings::set_live_status_quota_position(docked_pos);
            ctx.send_viewport_cmd_to(
                quota_viewport_id,
                egui::ViewportCommand::OuterPosition(egui::pos2(
                    docked_pos[0],
                    docked_pos[1],
                )),
            );
        }
        app.sync_corner_regions();
    }
    if let Some(docked) = detected_dock_change {
        app.quota_docked = docked;
        let _ = window_settings::set_live_status_quota_docked(docked);
        app.sync_corner_regions();
    }
    if let Some(pos) = detected_new_pos {
        app.quota_pos = Some(pos);
        let _ = window_settings::set_live_status_quota_position(pos);
    }
    if let Some(new_scale) = detected_scale_change {
        apply_live_status_scale(app, ctx, new_scale, true);
    }
}

fn format_reset_phrase(
    reset_raw: Option<&str>,
    is_five_hour: bool,
    language: Language,
) -> String {
    let dt = crate::quota::current_local_datetime();
    crate::quota::format_reset_time_phrase(
        reset_raw,
        is_five_hour,
        language,
        (dt.0 as i32, dt.1, dt.2, dt.4, dt.5),
    )
}

fn draw_smooth_limit_block(
    painter: &egui::Painter,
    top_left: egui::Pos2,
    width: f32,
    title: &str,
    pct: u8,
    reset_raw: Option<&str>,
    is_five_hour: bool,
    is_throttled: bool,
    forecast: &PacingForecast,
    language: Language,
) {
    // Line 1: Header (Left: Title + pct, Right: Runway tag)
    let title_text = format!("{title}: {pct}%");
    let title_galley = painter.layout_no_wrap(
        title_text.into(),
        egui::FontId::monospace(10.5),
        egui::Color32::from_rgb(226, 232, 240),
    );
    painter.galley(top_left, title_galley, egui::Color32::from_rgb(226, 232, 240));

    let runway_text = if is_five_hour {
        forecast.five_hour_runway_text(language)
    } else {
        forecast.week_runway_text(language)
    };

    // Simon's Traffic-Light Pastel Rule (A3 Whisper Jade #78b886):
    // Green (#78b886): Ample buffer (>30d, 5.7d, buffer stable). Alles OK!
    // Yellow (#fde047): Tight (<3.0d, high pace, moderate deficit). Vorwarn-Zone!
    // Red (#fca5a5): Critical (<1.0d, severe deficit >45m before reset, throttled, 0%). Problem!
    let (runway_color, is_red_alert, is_yellow_alert) = if pct == 0 || is_throttled {
        (egui::Color32::from_rgb(252, 165, 165), true, false)
    } else if is_five_hour {
        if let Some(fh) = &forecast.five_hour_forecast {
            if fh.is_exhausted_before_reset {
                let def = fh.delta_minutes.unwrap_or(0);
                if def < -45 {
                    (egui::Color32::from_rgb(252, 165, 165), true, false)
                } else {
                    (egui::Color32::from_rgb(253, 224, 71), false, true)
                }
            } else if pct < 20 || (pct < 30 && fh.pace_ratio > 1.15) {
                (egui::Color32::from_rgb(253, 224, 71), false, true)
            } else {
                (egui::Color32::from_rgb(120, 184, 134), false, false)
            }
        } else if pct < 20 {
            (egui::Color32::from_rgb(253, 224, 71), false, true)
        } else {
            (egui::Color32::from_rgb(120, 184, 134), false, false)
        }
    } else {
        let runway_days = forecast.runway_days.unwrap_or(30.0);
        if runway_days < 1.0 || pct < 10 {
            (egui::Color32::from_rgb(252, 165, 165), true, false)
        } else if runway_days < 3.0 || pct < 30 || forecast.week_is_deficit() {
            (egui::Color32::from_rgb(253, 224, 71), false, true)
        } else {
            (egui::Color32::from_rgb(120, 184, 134), false, false)
        }
    };

    let runway_galley = painter.layout_no_wrap(
        runway_text.into(),
        egui::FontId::monospace(10.0),
        runway_color,
    );
    let runway_x = top_left.x + width - runway_galley.size().x;
    painter.galley(
        egui::pos2(runway_x, top_left.y),
        runway_galley,
        runway_color,
    );

    // Line 2: Option A Single-Row Micro-Dots (2.2px diameter, like music progress bar)
    let track_y = top_left.y + 14.0;
    let dot_step = 3.6;
    let dot_radius = 1.1;
    let dot_count = ((width - 2.0) / dot_step).floor() as usize;
    let active_count = ((dot_count as f32) * (pct as f32 / 100.0)).round() as usize;

    let dot_active_color = if is_red_alert {
        egui::Color32::from_rgb(248, 113, 113) // Pastel coral red
    } else if is_yellow_alert {
        egui::Color32::from_rgb(251, 191, 36) // Warm amber
    } else {
        egui::Color32::from_rgb(157, 196, 118) // Sage green pastel (#9dc476)
    };
    let dot_unfilled_color = egui::Color32::from_rgba_unmultiplied(51, 65, 85, 130);

    for i in 0..dot_count {
        let dot_x = top_left.x + 1.1 + (i as f32) * dot_step;
        let dot_pos = egui::pos2(dot_x, track_y);
        let is_active = i < active_count;
        if is_active {
            if i + 1 == active_count {
                // Tip dot: micro-halo + white center
                painter.circle_filled(dot_pos, 2.4, dot_active_color.gamma_multiply(0.4));
                painter.circle_filled(dot_pos, dot_radius, egui::Color32::from_rgb(255, 255, 255));
            } else {
                painter.circle_filled(dot_pos, dot_radius, dot_active_color);
            }
        } else {
            painter.circle_filled(dot_pos, dot_radius, dot_unfilled_color);
        }
    }

    // Line 3: Footer meta
    let footer_left = if let Some(raw) = reset_raw {
        let reset_phrase = format_reset_phrase(Some(raw), is_five_hour, language);
        format!("Reset {reset_phrase}")
    } else {
        format!("{pct}% {}", language.text("frei", "free"))
    };
    let footer_left_galley = painter.layout_no_wrap(
        footer_left.into(),
        egui::FontId::monospace(9.5),
        egui::Color32::from_rgb(148, 163, 184),
    );
    let footer_y = track_y + 5.0;
    painter.galley(
        egui::pos2(top_left.x, footer_y),
        footer_left_galley,
        egui::Color32::from_rgb(148, 163, 184),
    );

    // Right: Tempo / Pace
    let (pace_text, pace_ratio_opt) = if is_five_hour {
        (
            forecast.five_hour_pace_text(language),
            forecast.five_hour_forecast.as_ref().map(|fh| fh.pace_ratio),
        )
    } else {
        (
            forecast.week_pace_text(language),
            forecast.pace_ratio,
        )
    };
    if !pace_text.is_empty() {
        let pace_color = if pct == 0 || is_throttled || is_red_alert {
            egui::Color32::from_rgb(252, 165, 165) // Red: throttled / depleted / red alert
        } else if is_yellow_alert {
            egui::Color32::from_rgb(253, 224, 71) // Yellow: yellow warning / tight buffer
        } else if let Some(ratio) = pace_ratio_opt {
            if ratio < 1.0 {
                egui::Color32::from_rgb(120, 184, 134) // Green: healthy pace / good reserve
            } else if ratio <= 1.25 {
                egui::Color32::from_rgb(253, 224, 71) // Yellow: slightly above 1.0
            } else {
                egui::Color32::from_rgb(252, 165, 165) // Red: far above 1.0
            }
        } else {
            egui::Color32::from_rgb(148, 163, 184)
        };

        let footer_right_galley = painter.layout_no_wrap(
            pace_text.into(),
            egui::FontId::monospace(9.5),
            pace_color,
        );
        let right_x = top_left.x + width - footer_right_galley.size().x;
        painter.galley(
            egui::pos2(right_x, footer_y),
            footer_right_galley,
            pace_color,
        );
    }
}

fn draw_provider_card(
    painter: &egui::Painter,
    card_rect: egui::Rect,
    quota: &ProviderQuota,
    forecast: &PacingForecast,
    language: Language,
    provider_color: egui::Color32,
) {
    // Card background
    painter.rect_filled(
        card_rect,
        egui::CornerRadius::same(6),
        egui::Color32::from_rgba_unmultiplied(18, 24, 38, 180),
    );
    painter.rect_stroke(
        card_rect,
        egui::CornerRadius::same(6),
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 90)),
        egui::StrokeKind::Inside,
    );

    // Pill badge colors
    let pill_color = provider_color;
    let pill_bg = egui::Color32::from_rgba_unmultiplied(pill_color.r(), pill_color.g(), pill_color.b(), 32);
    let pill_stroke = egui::Color32::from_rgba_unmultiplied(pill_color.r(), pill_color.g(), pill_color.b(), 140);

    // Header row
    let header_y = card_rect.top() + 6.0;
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(card_rect.left() + 8.0, header_y),
        egui::vec2(44.0, 18.0),
    );
    painter.rect_filled(badge_rect, egui::CornerRadius::same(3), pill_bg);
    painter.rect_stroke(badge_rect, egui::CornerRadius::same(3), egui::Stroke::new(1.0, pill_stroke), egui::StrokeKind::Inside);
    let badge_galley = painter.layout_no_wrap(
        quota.title.into(),
        egui::FontId::proportional(10.5),
        pill_color,
    );
    painter.galley(
        egui::pos2(
            badge_rect.center().x - badge_galley.size().x / 2.0,
            badge_rect.center().y - badge_galley.size().y / 2.0,
        ),
        badge_galley,
        pill_color,
    );

    // Full Name
    let full_name = match quota.id {
        ProviderId::Glm => "Z.ai · GLM-4",
        ProviderId::Agy => "Google · Antigravity",
        ProviderId::Codex => "OpenAI · Codex",
        ProviderId::Claude => "Anthropic · Claude",
    };
    let name_galley = painter.layout_no_wrap(
        full_name.into(),
        egui::FontId::proportional(11.5),
        egui::Color32::from_rgb(226, 232, 240),
    );
    painter.galley(
        egui::pos2(badge_rect.right() + 8.0, header_y + 1.0),
        name_galley,
        egui::Color32::from_rgb(226, 232, 240),
    );

    // Right: Pure status LED light without text clutter (per Simon's request)
    let (led_color, halo_color) = match forecast.health {
        PacingHealth::Surplus | PacingHealth::OnTrack => (
            egui::Color32::from_rgb(34, 197, 94),
            egui::Color32::from_rgba_unmultiplied(34, 197, 94, 70),
        ),
        PacingHealth::Tight => (
            egui::Color32::from_rgb(253, 224, 71),
            egui::Color32::from_rgba_unmultiplied(253, 224, 71, 75),
        ),
        PacingHealth::Throttled => (
            egui::Color32::from_rgb(239, 68, 68),
            egui::Color32::from_rgba_unmultiplied(239, 68, 68, 80),
        ),
    };
    let led_pos = egui::pos2(card_rect.right() - 14.0, header_y + 9.0);
    painter.circle_filled(led_pos, 5.0, halo_color);
    painter.circle_filled(led_pos, 2.5, led_color);
    painter.circle_filled(
        egui::pos2(led_pos.x - 0.7, led_pos.y - 0.7),
        0.8,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180),
    );

    // Subtle divider
    let div_y = header_y + 22.0;
    painter.line_segment(
        [
            egui::pos2(card_rect.left() + 8.0, div_y),
            egui::pos2(card_rect.right() - 8.0, div_y),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(65, 82, 115, 45)),
    );

    // Limit Blocks
    let inner_left = card_rect.left() + 10.0;
    let inner_w = card_rect.width() - 20.0;

    let has_5h = quota.five_hour_percent.is_some() || quota.five_hour_reset.is_some();
    let has_wk = quota.week_percent.is_some() || quota.week_reset.is_some();

    if has_5h && has_wk {
        let title_5h = if quota.id == ProviderId::Claude {
            language.text("Session-Limit", "Session Limit")
        } else {
            language.text("5h-Limit", "5-Hour Limit")
        };
        let five_h_throttled = quota.five_hour_percent.map(|p| p == 0).unwrap_or(false);
        draw_smooth_limit_block(
            painter,
            egui::pos2(inner_left, div_y + 5.0),
            inner_w,
            title_5h,
            quota.five_hour_percent.unwrap_or(100),
            quota.five_hour_reset.as_deref(),
            true,
            five_h_throttled,
            forecast,
            language,
        );

        let title_wk = if quota.cycle_label() == "Mo" {
            language.text("Monats-Limit", "Monthly Limit")
        } else {
            language.text("Wochen-Limit", "Weekly Limit")
        };
        let week_throttled = quota.week_percent.map(|p| p == 0).unwrap_or(false);
        draw_smooth_limit_block(
            painter,
            egui::pos2(inner_left, div_y + 41.0),
            inner_w,
            title_wk,
            quota.week_percent.unwrap_or(100),
            quota.week_reset.as_deref(),
            false,
            week_throttled,
            forecast,
            language,
        );
    } else if has_5h {
        let title_5h = if quota.id == ProviderId::Claude {
            language.text("Session-Limit", "Session Limit")
        } else {
            language.text("5h-Limit", "5-Hour Limit")
        };
        let five_h_throttled = quota.five_hour_percent.map(|p| p == 0).unwrap_or(false) || quota.is_throttled;
        draw_smooth_limit_block(
            painter,
            egui::pos2(inner_left, div_y + 5.0),
            inner_w,
            title_5h,
            quota.five_hour_percent.unwrap_or(100),
            quota.five_hour_reset.as_deref(),
            true,
            five_h_throttled,
            forecast,
            language,
        );
    } else if has_wk {
        let title_wk = if quota.cycle_label() == "Mo" {
            language.text("Monats-Limit", "Monthly Limit")
        } else {
            language.text("Wochen-Limit", "Weekly Limit")
        };
        let week_throttled = quota.week_percent.map(|p| p == 0).unwrap_or(false) || quota.is_throttled;
        draw_smooth_limit_block(
            painter,
            egui::pos2(inner_left, div_y + 5.0),
            inner_w,
            title_wk,
            quota.week_percent.unwrap_or(100),
            quota.week_reset.as_deref(),
            false,
            week_throttled,
            forecast,
            language,
        );
    } else {
        let none_galley = painter.layout_no_wrap(
            language.text("Kein Limit aktiv · Unbegrenzt", "No limit active · Unlimited").into(),
            egui::FontId::monospace(11.0),
            egui::Color32::from_rgb(148, 163, 184),
        );
        painter.galley(
            egui::pos2(inner_left, div_y + 10.0),
            none_galley,
            egui::Color32::from_rgb(148, 163, 184),
        );
    }
}

#[allow(dead_code)]
fn render_quota_tooltip(
    ui: &mut egui::Ui,
    quota: &ProviderQuota,
    forecast: &PacingForecast,
    language: Language,
) {
    ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

    // Header: Provider & Status
    ui.horizontal(|ui| {
        ui.strong(format!("{} · {}", quota.title, quota.author));
        let (status_text, status_color) = match forecast.health {
            PacingHealth::Surplus => (
                language.text("Reserve solide", "Reserve solid"),
                egui::Color32::from_rgb(34, 197, 94),
            ),
            PacingHealth::OnTrack => (
                language.text("Im Plan", "On track"),
                egui::Color32::from_rgb(56, 189, 248),
            ),
            PacingHealth::Tight => (
                language.text("Knapp / Erhöht", "Tight / High pace"),
                egui::Color32::from_rgb(245, 158, 11),
            ),
            PacingHealth::Throttled => (
                language.text("Gedrosselt", "Throttled"),
                egui::Color32::from_rgb(239, 68, 68),
            ),
        };
        ui.colored_label(status_color, format!("· {status_text}"));
    });

    ui.separator();

    // 5-Hour Section
    if let Some(pct) = quota.five_hour_percent {
        let title_5h = if quota.id == ProviderId::Claude {
            language.text("Session-Fenster (3-5h)", "Session Window (3-5h)")
        } else {
            language.text("5-Stunden-Sitzungsfenster", "5-Hour Session Window")
        };

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title_5h).strong().color(egui::Color32::from_rgb(226, 232, 240)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(egui::Color32::from_rgb(157, 196, 118), format!("{pct}% {}", language.text("verfügbar", "remaining")));
            });
        });

        egui::Grid::new(format!("grid_5h_{:?}", quota.id))
            .num_columns(2)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new(language.text("Reset:", "Reset:")).color(egui::Color32::from_rgb(148, 163, 184)));
                let reset_str = quota.five_hour_reset.as_deref().unwrap_or("—");
                let phrase = format_reset_phrase(quota.five_hour_reset.as_deref(), true, language);
                let label_text = if phrase.contains(reset_str) || reset_str == "—" {
                    phrase
                } else {
                    format!("{phrase} ({reset_str})")
                };
                ui.label(label_text);
                ui.end_row();

                if let Some(fh) = &forecast.five_hour_forecast {
                    ui.label(egui::RichText::new(language.text("Pace:", "Pace:")).color(egui::Color32::from_rgb(148, 163, 184)));
                    let pace_color = if fh.is_exhausted_before_reset && fh.delta_minutes.unwrap_or(0) < -45 {
                        egui::Color32::from_rgb(252, 165, 165)
                    } else if fh.pace_ratio < 1.0 {
                        egui::Color32::from_rgb(120, 184, 134)
                    } else if fh.pace_ratio <= 1.25 {
                        egui::Color32::from_rgb(253, 224, 71)
                    } else {
                        egui::Color32::from_rgb(252, 165, 165)
                    };
                    ui.colored_label(pace_color, forecast.five_hour_pace_text(language));
                    ui.end_row();

                    ui.label(egui::RichText::new(language.text("Reichweite:", "Runway:")).color(egui::Color32::from_rgb(148, 163, 184)));
                    let rw_color = if fh.is_exhausted_before_reset {
                        egui::Color32::from_rgb(245, 158, 11)
                    } else {
                        egui::Color32::from_rgb(56, 189, 248)
                    };
                    ui.colored_label(rw_color, forecast.five_hour_runway_text(language));
                    ui.end_row();

                    ui.label(egui::RichText::new(language.text("Reserve-Lage:", "Reserve status:")).color(egui::Color32::from_rgb(148, 163, 184)));
                    if fh.is_exhausted_before_reset {
                        let def_mins = fh.delta_minutes.map(|d| (-d).max(1)).unwrap_or(30);
                        let (h, m) = fh.exhaustion_time.unwrap_or((0, 0));
                        ui.colored_label(
                            egui::Color32::from_rgb(239, 68, 68),
                            format!("-{}m ({} {:02}:{:02})", def_mins, language.text("Leer um", "Empty at"), h, m),
                        );
                    } else {
                        let buf_text = if let Some(delta) = fh.delta_minutes {
                            let h = delta / 60;
                            let m = delta % 60;
                            if h > 0 && m > 0 {
                                format!("+{}h {}m {}", h, m, language.text("über Reset hinaus", "beyond reset"))
                            } else if h > 0 {
                                format!("+{}h {}", h, language.text("über Reset hinaus", "beyond reset"))
                            } else {
                                format!("+{}m {}", m, language.text("über Reset hinaus", "beyond reset"))
                            }
                        } else {
                            language.text("+>24h Reserve", "+>24h reserve").to_string()
                        };
                        ui.colored_label(egui::Color32::from_rgb(157, 196, 118), buf_text);
                    }
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
    }

    // Weekly / Monthly Section
    if let Some(pct) = quota.week_percent {
        if quota.five_hour_percent.is_some() {
            ui.separator();
        }

        let is_monthly = quota.cycle_label() == "Mo";
        let title_wk = if is_monthly {
            language.text("Monats-Kontingent", "Monthly Quota")
        } else {
            language.text("Wochen-Kontingent", "Weekly Quota")
        };

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title_wk).strong().color(egui::Color32::from_rgb(226, 232, 240)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(egui::Color32::from_rgb(157, 196, 118), format!("{pct}% {}", language.text("verfügbar", "remaining")));
            });
        });

        egui::Grid::new(format!("grid_wk_{:?}", quota.id))
            .num_columns(2)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new(language.text("Reset:", "Reset:")).color(egui::Color32::from_rgb(148, 163, 184)));
                let reset_str = quota.week_reset.as_deref().unwrap_or("—");
                let phrase = format_reset_phrase(quota.week_reset.as_deref(), false, language);
                let label_text = if phrase.contains(reset_str) || reset_str == "—" {
                    phrase
                } else {
                    format!("{phrase} ({reset_str})")
                };
                ui.label(label_text);
                ui.end_row();

                if let Some(ratio) = forecast.pace_ratio {
                    ui.label(egui::RichText::new(language.text("Pace:", "Pace:")).color(egui::Color32::from_rgb(148, 163, 184)));
                    let pace_color = if forecast.week_is_deficit() {
                        egui::Color32::from_rgb(252, 165, 165)
                    } else if ratio < 1.0 {
                        egui::Color32::from_rgb(120, 184, 134)
                    } else if ratio <= 1.25 {
                        egui::Color32::from_rgb(253, 224, 71)
                    } else {
                        egui::Color32::from_rgb(252, 165, 165)
                    };
                    ui.colored_label(pace_color, forecast.week_pace_text(language));
                    ui.end_row();
                }

                ui.label(egui::RichText::new(language.text("Reichweite:", "Runway:")).color(egui::Color32::from_rgb(148, 163, 184)));
                let rw_color = if forecast.health == PacingHealth::Tight || forecast.health == PacingHealth::Throttled {
                    egui::Color32::from_rgb(245, 158, 11)
                } else {
                    egui::Color32::from_rgb(56, 189, 248)
                };
                ui.colored_label(rw_color, forecast.week_runway_text(language));
                ui.end_row();

                ui.label(egui::RichText::new(language.text("Reserve-Lage:", "Reserve status:")).color(egui::Color32::from_rgb(148, 163, 184)));
                let (status_buf, buf_color) = if forecast.health == PacingHealth::Throttled {
                    (language.text("Erschöpft", "Depleted").to_string(), egui::Color32::from_rgb(252, 165, 165))
                } else if let Some(rw) = forecast.runway_days {
                    let diff = rw - forecast.remaining_days;
                    if diff < -0.04 {
                        if diff <= -1.5 {
                            (format!("-{} {}", (-diff).round() as i32, language.text("Tage Defizit", "days deficit")), egui::Color32::from_rgb(252, 165, 165))
                        } else if diff <= -0.85 {
                            (format!("-1 {}", language.text("Tag Defizit", "day deficit")), egui::Color32::from_rgb(252, 165, 165))
                        } else {
                            let h = ((-diff) * 24.0).round() as i32;
                            (format!("-{}h {}", h.max(1), language.text("Defizit", "deficit")), egui::Color32::from_rgb(252, 165, 165))
                        }
                    } else if diff > 0.04 {
                        if diff > 14.0 {
                            (language.text("+>30 Tage Reserve", "+>30 days reserve").to_string(), egui::Color32::from_rgb(120, 184, 134))
                        } else if diff > 7.0 {
                            (language.text("+>7 Tage Reserve", "+>7 days reserve").to_string(), egui::Color32::from_rgb(120, 184, 134))
                        } else if diff >= 1.5 {
                            (format!("+{} {}", diff.round() as i32, language.text("Tage Reserve", "days reserve")), egui::Color32::from_rgb(120, 184, 134))
                        } else if diff >= 0.85 {
                            (format!("+1 {}", language.text("Tag Reserve", "day reserve")), egui::Color32::from_rgb(120, 184, 134))
                        } else {
                            let h = (diff * 24.0).round() as i32;
                            (format!("+{}h {}", h.max(1), language.text("Reserve", "reserve")), egui::Color32::from_rgb(120, 184, 134))
                        }
                    } else {
                        (language.text("±0h Reserve", "±0h reserve").to_string(), egui::Color32::from_rgb(120, 184, 134))
                    }
                } else if let Some(delta) = forecast.delta_days {
                    if delta < 0 {
                        (format!("-{} {}", -delta, language.text("Tage Defizit", "days deficit")), egui::Color32::from_rgb(252, 165, 165))
                    } else if delta > 14 {
                        (language.text("+>30 Tage Reserve", "+>30 days reserve").to_string(), egui::Color32::from_rgb(120, 184, 134))
                    } else {
                        (format!("+{} {}", delta, language.text("Tage Reserve", "days reserve")), egui::Color32::from_rgb(120, 184, 134))
                    }
                } else {
                    (language.text("+>30 Tage Reserve", "+>30 days reserve").to_string(), egui::Color32::from_rgb(120, 184, 134))
                };
                ui.colored_label(buf_color, status_buf);
                ui.end_row();
            });
    }
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

#[allow(clippy::too_many_arguments)]
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
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        CLOCK_CARDINAL_FONT_NAME.to_owned(),
        Arc::new(egui::FontData::from_static(RUSSO_ONE_FONT_BYTES)),
    );
    fonts.families.insert(
        egui::FontFamily::Name(Arc::from(CLOCK_CARDINAL_FONT_NAME)),
        vec![CLOCK_CARDINAL_FONT_NAME.to_owned()],
    );
    context.set_fonts(fonts);

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

    static LIVE_OPEN_ATTEMPT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_open_attempt_mutex_name(case: &str) -> String {
        format!(
            "Local\\HerdrNachtwaechter.LiveOpenAttempt.Test.{}.{}",
            std::process::id(),
            case
        )
    }

    fn fixture_output_confirms_exact_test(output: &str, fixture: &str) -> bool {
        output.contains("running 1 test") && output.contains(&format!("test {fixture} ... ok"))
    }

    #[test]
    fn window_control_hood_is_fully_opaque() {
        assert_eq!(WINDOW_CONTROL_HOOD_FILL.to_array()[3], u8::MAX);
    }

    fn assert_fixture_output(output: std::process::Output, fixture: &str) {
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "fixture {fixture} failed: {transcript}"
        );
        assert!(
            fixture_output_confirms_exact_test(&transcript, fixture),
            "fixture {fixture} did not run exactly once: {transcript}"
        );
    }

    #[test]
    fn saved_position_on_an_attached_monitor_stays_untouched() {
        let areas = [
            WorkArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1392,
            },
            WorkArea {
                left: -2560,
                top: 0,
                right: 0,
                bottom: 1392,
            },
        ];
        assert_eq!(
            clamp_live_position(Some([-595.0, 41.0]), [393.0, 190.0], &areas),
            Some([-595.0, 41.0])
        );
    }

    #[test]
    fn position_of_a_missing_monitor_clamps_to_nearest_work_area() {
        let areas = [
            WorkArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1392,
            },
            WorkArea {
                left: -2560,
                top: 0,
                right: 0,
                bottom: 1392,
            },
        ];
        assert_eq!(
            clamp_live_position(Some([-3000.0, 41.0]), [393.0, 190.0], &areas),
            Some([-2560.0, 41.0])
        );
    }

    #[test]
    fn clamped_position_keeps_the_window_inside_the_work_area() {
        let areas = [WorkArea {
            left: 0,
            top: 0,
            right: 300,
            bottom: 200,
        }];
        assert_eq!(
            clamp_live_position(Some([900.0, 150.0]), [393.0, 190.0], &areas),
            Some([0.0, 10.0])
        );
    }

    #[test]
    fn origin_inside_an_area_still_clamps_an_oversized_window() {
        let areas = [WorkArea {
            left: 0,
            top: 0,
            right: 300,
            bottom: 200,
        }];
        // The origin is inside the area, but a 393 px wide window would stick
        // out over the right edge, so the window is pulled back inside on
        // both axes.
        assert_eq!(
            clamp_live_position(Some([150.0, 41.0]), [393.0, 190.0], &areas),
            Some([0.0, 10.0])
        );
    }

    #[test]
    fn missing_areas_or_missing_save_use_default_placement() {
        let areas = [WorkArea {
            left: 0,
            top: 0,
            right: 2560,
            bottom: 1392,
        }];
        assert_eq!(
            clamp_live_position(Some([10.0, 10.0]), [393.0, 190.0], &[]),
            None
        );
        assert_eq!(clamp_live_position(None, [393.0, 190.0], &areas), None);
        assert_eq!(
            clamp_live_position(Some([f32::NAN, 10.0]), [393.0, 190.0], &areas),
            None
        );
    }

    #[test]
    fn extreme_coordinates_pick_the_nearest_area_without_overflow() {
        let areas = [
            WorkArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1392,
            },
            WorkArea {
                left: -2560,
                top: 0,
                right: 0,
                bottom: 1392,
            },
        ];
        // f32::MIN saturates to i32::MIN on the cast; the left monitor stays
        // the nearest one and the arithmetic must not overflow.
        assert_eq!(
            clamp_live_position(Some([f32::MIN, 20.0]), [393.0, 190.0], &areas),
            Some([-2560.0, 20.0])
        );
    }

    #[test]
    fn physical_work_area_is_converted_before_clamping_at_150_percent() {
        let area = WorkArea::from_physical_rect(
            RECT {
                left: 0,
                top: 0,
                right: 3840,
                bottom: 2160,
            },
            1.5,
        );

        assert_eq!(
            area,
            WorkArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            }
        );
        assert_eq!(
            clamp_live_position(Some([3000.0, 100.0]), [393.0, 190.0], &[area]),
            Some([2167.0, 100.0])
        );
    }

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
    fn clock_temperature_uses_the_free_space_above_the_dial() {
        let center = egui::pos2(100.0, 100.0);
        let radius = 30.0;
        let temperature = clock_temperature_position(center, radius);

        assert!(temperature.y < center.y - radius * MOON_HALO_OUTER_RADIUS);
        assert!(temperature.y > center.y - radius * 2.4);
        assert_eq!(CLOCK_TEMPERATURE_VERTICAL_OFFSET, 2.0);
        assert_eq!(CLOCK_TEMPERATURE_FONT_SIZE, 11.0);
        assert_eq!(CLOCK_WEATHER_SYMBOL_SIZE, 32.0);
        assert!(CLOCK_WEATHER_SYMBOL_SIZE >= 15.0 * 2.0);
    }

    #[test]
    fn clock_hands_point_to_twelve_and_three_on_the_hour() {
        let midnight = clock_hand_turns(0, 0, 0, 0);
        assert!((midnight.hour - 0.0).abs() < f32::EPSILON);
        assert!((midnight.minute - 0.0).abs() < f32::EPSILON);
        assert!((midnight.second - 0.0).abs() < f32::EPSILON);

        let three_oclock = clock_hand_turns(3, 0, 0, 0);
        assert!((three_oclock.hour - 0.25).abs() < f32::EPSILON);
        assert!((three_oclock.minute - 0.0).abs() < f32::EPSILON);
        assert!((three_oclock.second - 0.0).abs() < f32::EPSILON);

        let half_past_three = clock_hand_turns(3, 30, 30, 500);
        assert!((half_past_three.second - 30.5 / 60.0).abs() < 0.0001);
        assert!((half_past_three.minute - (30.0 + 30.5 / 60.0) / 60.0).abs() < 0.0001);
        assert!((half_past_three.hour - (3.0 + (30.0 + 30.5 / 60.0) / 60.0) / 12.0).abs() < 0.0001);
        assert!(
            (clock_hand_turns(23, 0, 0, 0).hour - clock_hand_turns(11, 0, 0, 0).hour).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn clock_dial_replaces_quarter_hour_marks_with_cardinal_labels() {
        let marks = clock_index_mark_specs();

        assert_eq!(marks.len(), 8);
        for mark in marks {
            let hour = (mark.turns * 12.0).round() as u8;
            assert_ne!(hour % 3, 0);
            assert_eq!(mark.width, 2.0);
            assert_eq!(mark.height, 3.5);
        }

        let labels = clock_cardinal_label_specs();
        assert_eq!(labels.map(|label| label.text), ["12", "3", "6", "9"]);
        assert_eq!(labels.map(|label| label.turns), [0.0, 0.25, 0.5, 0.75]);
    }

    #[test]
    fn clock_index_marks_stay_outside_the_moon_and_inside_its_halo() {
        let center = egui::Pos2::ZERO;
        let radius = 30.0;
        let [inner, outer] = clock_index_mark_segment(center, radius, 0.0, 3.5);

        assert!(inner.distance(center) > radius);
        assert!((outer.distance(inner) - 3.5).abs() < 0.001);
        assert!(outer.distance(center) < radius * MOON_HALO_OUTER_RADIUS);
    }

    #[test]
    fn clock_cardinal_numerals_stay_centered_in_the_expanded_moon_halo() {
        let center = egui::Pos2::ZERO;
        let radius = 30.0;

        for label in clock_cardinal_label_specs() {
            let position = clock_cardinal_label_position(center, radius, label.turns);
            assert!(position.distance(center) > radius);
            assert!(position.distance(center) < radius * MOON_HALO_OUTER_RADIUS);
        }
        assert_eq!(MOON_HALO_OUTER_RADIUS, 1.35);
    }

    #[test]
    fn clock_cardinal_numerals_use_russo_one_at_one_and_a_half_times_size() {
        assert_eq!(CLOCK_CARDINAL_FONT_NAME, "russo_one");
        assert_eq!(CLOCK_CARDINAL_FONT_SIZE, 9.3);
        assert!(RUSSO_ONE_FONT_BYTES.len() > 30_000);
    }

    #[test]
    fn moon_moves_thirteen_and_a_half_points_right_for_balanced_outer_spacing() {
        assert_eq!(MOON_RIGHT_INSET + MOON_SLOT_WIDTH / 2.0, 55.0);
    }

    #[test]
    fn needle_hand_flares_from_a_narrow_root_before_its_exact_tip() {
        let points = needle_hand_points(egui::Pos2::ZERO, 10.0, 0.0, 0.96, 4.0);

        assert_eq!(points.len(), 5);
        assert!(points[0].distance(egui::pos2(-0.96, 0.0)) < 0.001);
        assert!(points[1].distance(egui::pos2(-2.0, -7.68)) < 0.001);
        assert!(points[2].distance(egui::pos2(0.0, -9.6)) < 0.001);
        assert!(points[3].distance(egui::pos2(2.0, -7.68)) < 0.001);
        assert!(points[4].distance(egui::pos2(0.96, 0.0)) < 0.001);
    }

    #[test]
    fn both_main_hands_use_green_with_white_and_black_contours() {
        assert_eq!(MAIN_HAND_COLOR, PASTEL_GREEN);
        assert_eq!(
            MAIN_HAND_INNER_OUTLINE,
            egui::Color32::from_rgb(247, 241, 229)
        );
        assert_eq!(MAIN_HAND_OUTER_OUTLINE, egui::Color32::from_rgb(5, 8, 15));
    }

    #[test]
    fn green_moon_uses_yellow_main_hands_only_in_night_mode() {
        assert_eq!(main_hand_color_for_moon(GREEN), NIGHT_MODE_MAIN_HAND_COLOR);
        assert_eq!(main_hand_color_for_moon(PASTEL_YELLOW), MAIN_HAND_COLOR);
        assert_eq!(NIGHT_MODE_MAIN_HAND_COLOR, PASTEL_YELLOW);
    }

    #[test]
    fn main_hands_reach_beyond_the_temperature_and_to_the_outer_edge() {
        assert_eq!(HOUR_HAND_LENGTH, 0.72);
        assert_eq!(MINUTE_HAND_LENGTH, 0.96);
    }

    #[test]
    fn second_hand_is_a_long_slim_mudmaster_style_needle() {
        let points = second_hand_points(egui::Pos2::ZERO, 10.0, 0.0);

        assert_eq!(SECOND_HAND_SHAFT_WIDTH, 0.75);
        assert_eq!(SECOND_HAND_COLOR, egui::Color32::from_rgb(222, 142, 92));
        assert!(points[0].distance(egui::pos2(-0.375, 1.6)) < 0.001);
        assert!(points[1].distance(egui::pos2(-0.375, -8.8)) < 0.001);
        assert!(points[2].distance(egui::pos2(0.0, -10.2)) < 0.001);
        assert!(points[3].distance(egui::pos2(0.375, -8.8)) < 0.001);
        assert!(points[4].distance(egui::pos2(0.375, 1.6)) < 0.001);
    }

    #[test]
    fn context_menu_closes_after_every_setting_selection() {
        assert!(context_menu_should_close_after_selection(
            true, false, false, false
        ));
        assert!(context_menu_should_close_after_selection(
            false, true, false, false
        ));
        assert!(context_menu_should_close_after_selection(
            false, false, true, false
        ));
        assert!(context_menu_should_close_after_selection(
            false, false, false, true
        ));
        assert!(!context_menu_should_close_after_selection(
            false, false, false, false
        ));
    }

    #[test]
    fn context_menu_allows_the_clock_but_excludes_adjacent_controls() {
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(393.0, 190.0));

        assert!(is_live_context_menu_position(
            window,
            egui::pos2(333.0, 72.0)
        ));
        assert!(!is_live_context_menu_position(
            window,
            egui::pos2(220.0, 30.0)
        ));
    }

    #[test]
    fn temperature_position_stays_centered_above_the_halo() {
        let center = egui::pos2(50.0, 75.0);
        let position = clock_temperature_position(center, 20.0);

        assert_eq!(position.x, center.x);
        assert!(position.y < center.y - 20.0 * MOON_HALO_OUTER_RADIUS);
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
    fn satellite_window_titles_are_recognized() {
        assert!(is_satellite_window_title("Herdr-Nachtwächter - Limits"));
        assert!(is_satellite_window_title("Herdr Night Watch - Limits"));
        assert!(!is_satellite_window_title(""));
        assert!(!is_satellite_window_title("Herdr-Nachtwächter - Live-Status"));
        assert!(!is_satellite_window_title(
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
    fn existing_unready_window_is_recovered_unless_this_process_watches_it() {
        assert_eq!(
            existing_window_disposition(true, Some(41), None),
            ExistingWindowDisposition::Activate
        );
        assert_eq!(
            existing_window_disposition(false, Some(41), Some(41)),
            ExistingWindowDisposition::WaitForActiveAttempt
        );
        assert_eq!(
            existing_window_disposition(false, Some(41), Some(99)),
            ExistingWindowDisposition::Replace(Some(41))
        );
        assert_eq!(
            existing_window_disposition(false, None, None),
            ExistingWindowDisposition::Replace(None)
        );
    }

    #[test]
    #[ignore]
    fn secondary_process_observes_active_open_attempt_fixture() {
        if std::env::var_os("HERDR_LIVE_STATUS_FIXTURE").is_none() {
            return;
        }
        assert!(begin_open_spawn().is_none());
    }

    #[test]
    #[ignore]
    fn shared_open_attempt_gate_parent_fixture() {
        if std::env::var_os("HERDR_LIVE_STATUS_FIXTURE").is_none() {
            return;
        }
        let attempt_id = begin_open_spawn().unwrap();
        let fixture = "live_status::tests::secondary_process_observes_active_open_attempt_fixture";
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", fixture])
            .env("HERDR_LIVE_STATUS_FIXTURE", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        finish_open_spawn(attempt_id);

        assert_fixture_output(output, fixture);
    }

    #[test]
    #[ignore]
    fn stale_open_attempt_parent_fixture() {
        if std::env::var_os("HERDR_LIVE_STATUS_FIXTURE").is_none() {
            return;
        }
        let first_attempt = begin_open_spawn().unwrap();
        let newer_attempt = first_attempt + 1;
        {
            let mut gate = OPEN_ATTEMPT.lock().unwrap();
            gate.as_mut().unwrap().id = newer_attempt;
        }

        assert!(!remember_open_child(first_attempt, 4242));
        finish_open_spawn(first_attempt);
        assert_eq!(
            OPEN_ATTEMPT
                .lock()
                .unwrap()
                .as_ref()
                .map(|attempt| attempt.id),
            Some(newer_attempt)
        );

        finish_open_spawn(newer_attempt);
    }

    #[test]
    #[ignore]
    fn clean_child_exit_fixture() {}

    #[test]
    fn clean_child_exit_before_ready_does_not_retry() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "live_status::tests::clean_child_exit_fixture",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();

        assert!(matches!(
            await_live_window_ready(&mut child),
            LiveWatchOutcome::ChildExitedCleanly
        ));
    }

    #[test]
    fn duplicate_instance_without_ready_window_requests_retry() {
        let hwnd = 41usize as HWND;
        assert_eq!(
            duplicate_instance_disposition(None, |_| false),
            DuplicateInstanceDisposition::Retry
        );
        assert_eq!(
            duplicate_instance_disposition(Some(hwnd), |_| false),
            DuplicateInstanceDisposition::Retry
        );
        assert_eq!(
            duplicate_instance_disposition(Some(hwnd), |_| true),
            DuplicateInstanceDisposition::Activate(hwnd)
        );
    }

    #[test]
    fn timeout_details_follow_their_controlling_constants() {
        assert!(
            no_window_timeout_detail().contains(&LIVE_WINDOW_START_TIMEOUT.as_secs().to_string())
        );
        assert!(
            unpainted_timeout_detail().contains(&LIVE_WINDOW_VISIBLE_TIMEOUT.as_secs().to_string())
        );
    }

    #[test]
    fn fixture_output_must_name_one_executed_test() {
        let expected = "live_status::tests::example_fixture";
        assert!(fixture_output_confirms_exact_test(
            "running 1 test\ntest live_status::tests::example_fixture ... ok",
            expected
        ));
        assert!(!fixture_output_confirms_exact_test(
            "running 0 tests\ntest result: ok",
            expected
        ));
        assert!(!fixture_output_confirms_exact_test(
            "running 1 test\ntest live_status::tests::different_fixture ... ok",
            expected
        ));
    }

    #[test]
    fn open_attempt_gate_is_shared_across_processes() {
        let _serialized = LIVE_OPEN_ATTEMPT_TEST_LOCK.lock().unwrap();
        let fixture = "live_status::tests::shared_open_attempt_gate_parent_fixture";
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", fixture])
            .env("HERDR_LIVE_STATUS_FIXTURE", "1")
            .env(
                LIVE_OPEN_ATTEMPT_MUTEX_ENV,
                test_open_attempt_mutex_name("shared"),
            )
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();

        assert_fixture_output(output, fixture);
    }

    #[test]
    fn ignored_child_fixtures_are_inert_without_parent_marker() {
        for fixture in [
            "live_status::tests::secondary_process_observes_active_open_attempt_fixture",
            "live_status::tests::shared_open_attempt_gate_parent_fixture",
            "live_status::tests::stale_open_attempt_parent_fixture",
            "live_status::tests::terminate_and_reap_child_fixture",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", fixture])
                .env_remove("HERDR_LIVE_STATUS_FIXTURE")
                .env(
                    LIVE_OPEN_ATTEMPT_MUTEX_ENV,
                    test_open_attempt_mutex_name("inert"),
                )
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .unwrap();
            assert_fixture_output(output, fixture);
        }
    }

    #[test]
    fn stale_attempt_cannot_clear_or_overwrite_a_newer_attempt() {
        let _serialized = LIVE_OPEN_ATTEMPT_TEST_LOCK.lock().unwrap();
        let fixture = "live_status::tests::stale_open_attempt_parent_fixture";
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", fixture])
            .env("HERDR_LIVE_STATUS_FIXTURE", "1")
            .env(
                LIVE_OPEN_ATTEMPT_MUTEX_ENV,
                test_open_attempt_mutex_name("stale"),
            )
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();

        assert_fixture_output(output, fixture);
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
    fn eframe_owns_the_initial_hidden_until_painted_lifecycle() {
        let viewport = live_viewport_builder(1.0, None, Language::German);

        assert_ne!(viewport.visible, Some(false));
    }

    #[test]
    fn panic_and_open_failure_events_stay_distinguishable() {
        assert_ne!(LIVE_STATUS_PANIC_EVENT, LIVE_STATUS_OPEN_FAILURE_EVENT);
    }

    #[test]
    fn missing_refresh_timestamp_is_due_immediately() {
        assert!(refresh_is_due(
            None,
            Duration::from_secs(600),
            Instant::now(),
        ));
    }

    #[test]
    fn guard_retry_outlives_the_regular_spawn_retry() {
        // A post-open death points at the graphics stack still waking up, so
        // its retry must give the driver more time than a plain respawn.
        assert!(LIVE_WINDOW_GUARD_RETRY_DELAY > LIVE_WINDOW_RETRY_DELAY);
    }

    #[test]
    fn final_live_window_attempt_cannot_spawn_an_unwatched_child() {
        assert!(live_window_attempt_can_retry(1));
        assert!(live_window_attempt_can_retry(2));
        assert!(!live_window_attempt_can_retry(LIVE_WINDOW_START_ATTEMPTS));
    }

    #[test]
    #[ignore]
    fn terminate_and_reap_child_fixture() {
        if std::env::var_os("HERDR_LIVE_STATUS_FIXTURE").is_none() {
            return;
        }
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn terminate_and_reap_child_stops_the_process() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "live_status::tests::terminate_and_reap_child_fixture",
            ])
            .env("HERDR_LIVE_STATUS_FIXTURE", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();

        terminate_and_reap_child(&mut child);

        assert!(child.try_wait().unwrap().is_some());
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

    #[test]
    fn test_quota_satellite_design_height() {
        assert_eq!(quota_satellite_design_height(&[], true), 314.0);
        assert_eq!(quota_satellite_design_height(&[], false), 88.0);

        let glm = ProviderQuota::new(ProviderId::Glm, "GLM", "Z.ai", None, None, Some(90), Some("01.10.".into()));
        assert_eq!(provider_card_height(&glm), 68.0);
        assert_eq!(quota_satellite_design_height(&[glm.clone()], false), 38.0 + 68.0 + 8.0);

        let agy = ProviderQuota::new(ProviderId::Agy, "AGY", "Google", Some(73), Some("19:45".into()), Some(85), Some("137h".into()));
        assert_eq!(provider_card_height(&agy), 104.0);
        assert_eq!(quota_satellite_design_height(&[glm, agy], false), 38.0 + 68.0 + 104.0 + 6.0 + 8.0);
    }

    #[test]
    fn test_pomodoro_minutes_from_vector() {
        // 3 o'clock -> 15 min
        assert_eq!(pomodoro_minutes_from_vector(egui::vec2(10.0, 0.0)), 15.0);
        // 6 o'clock -> 30 min
        assert_eq!(pomodoro_minutes_from_vector(egui::vec2(0.0, 10.0)), 30.0);
        // 8 o'clock -> 40 min (Simon's default case)
        let angle_40m = (40.0 / 60.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let vec_40m = egui::vec2(angle_40m.cos() * 10.0, angle_40m.sin() * 10.0);
        assert_eq!(pomodoro_minutes_from_vector(vec_40m), 40.0);
        // 9 o'clock -> 45 min
        assert_eq!(pomodoro_minutes_from_vector(egui::vec2(-10.0, 0.0)), 45.0);
    }

    #[test]
    fn test_provider_pastel_palette() {
        assert_eq!(PROVIDER_PASTEL_PALETTE.len(), 10);
        for (i, def) in PROVIDER_PASTEL_PALETTE.iter().enumerate() {
            assert!(!def.name_de.is_empty(), "Palette {i} missing German name");
            assert!(!def.name_en.is_empty(), "Palette {i} missing English name");
            assert_ne!(def.color, egui::Color32::BLACK);
            assert_ne!(def.color, egui::Color32::TRANSPARENT);
        }
        // Unique colors
        let mut unique = std::collections::HashSet::new();
        for def in &PROVIDER_PASTEL_PALETTE {
            assert!(unique.insert(def.color), "Duplicate color in palette");
        }
        // Wrap-around test
        assert_eq!(provider_palette_color(0), PROVIDER_PASTEL_PALETTE[0].color);
        assert_eq!(provider_palette_color(9), PROVIDER_PASTEL_PALETTE[9].color);
        assert_eq!(provider_palette_color(10), PROVIDER_PASTEL_PALETTE[0].color);
    }
}
