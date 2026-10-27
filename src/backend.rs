use crate::configuration;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionAction {
    Sleep,
    Shutdown,
}

impl CompletionAction {
    fn from_wire(value: &str) -> Self {
        match value {
            "sleep" => Self::Sleep,
            _ => Self::Shutdown,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Self::Sleep => "sleep",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchStatus {
    Off {
        agents: AgentSummary,
        completion_action: CompletionAction,
        warning_seconds: u64,
    },
    Watching {
        targets: usize,
        live_scope: bool,
        observe_only: bool,
        quiet: bool,
        demo: bool,
        agents: AgentSummary,
        completion_action: CompletionAction,
        warning_seconds: u64,
    },
    ShutdownWarning {
        warning_id: String,
        targets: usize,
        live_scope: bool,
        demo: bool,
        observe_only: bool,
        warning_seconds: u64,
        seconds_remaining: u64,
        agents: AgentSummary,
        completion_action: CompletionAction,
        network_triggered: bool,
    },
    Finished {
        outcome: String,
        agents: AgentSummary,
        completion_action: CompletionAction,
        warning_seconds: u64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSummary {
    pub available: bool,
    pub total: usize,
    pub working: usize,
    pub idle: usize,
    pub done: usize,
    pub blocked: usize,
    pub unknown: usize,
}

#[derive(Debug, Deserialize)]
struct StatusPayload {
    state: Option<RunState>,
    shutdown_warning: Option<ShutdownWarning>,
    #[serde(default)]
    live_agents: LiveAgentsPayload,
    #[serde(default)]
    preferred_completion_action: String,
    #[serde(default)]
    preferred_warning_seconds: u64,
}

#[derive(Debug, Deserialize, Default)]
struct LiveAgentsPayload {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    working: usize,
    #[serde(default)]
    idle: usize,
    #[serde(default)]
    done: usize,
    #[serde(default)]
    blocked: usize,
    #[serde(default)]
    unknown: usize,
}

#[derive(Debug, Deserialize)]
struct RunState {
    #[serde(default)]
    demo: bool,
    dry_run: bool,
    warning_seconds: u64,
    all_terminal_since: Option<String>,
    outcome: Option<String>,
    #[serde(default)]
    monitoring_scope: String,
    #[serde(default)]
    completion_action: String,
    targets: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Target {
    pane_id: String,
}

#[derive(Debug, Deserialize)]
struct ShutdownWarning {
    #[serde(default)]
    warning_id: String,
    cancelable_until: String,
    #[serde(default)]
    seconds_remaining: u64,
    #[serde(default)]
    reason: String,
}

fn watcher_path() -> String {
    configuration::load().watcher_path
}

fn logs_dir_for_wsl() -> Option<String> {
    let directory = std::env::current_exe().ok()?.parent()?.join("logs");
    let display = directory.to_string_lossy().replace('\\', "/");
    let mut chars = display.chars();
    let drive = chars.next()?;
    if chars.next() != Some(':') || !drive.is_ascii_alphabetic() {
        return None;
    }
    let rest = display.get(3..).unwrap_or_default().trim_start_matches('/');
    Some(format!("/mnt/{}/{rest}", drive.to_ascii_lowercase()))
}

fn wsl() -> Command {
    let mut command = Command::new("wsl.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(configuration::load().distro)
        .arg("--exec");
    if let Some(logs) = logs_dir_for_wsl() {
        command.env("HERDR_NIGHT_WATCH_LOG_DIR", logs);
    }
    command
}

fn command_output(mut command: Command, label: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("{label} konnte nicht gestartet werden"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{label} fehlgeschlagen{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn spawn_watcher() -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--watch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .context("Herdr-Hintergrundwächter konnte nicht gestartet werden")?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(status) = child
            .try_wait()
            .context("Herdr-Hintergrundwächter konnte nicht geprüft werden")?
        {
            bail!("Herdr-Hintergrundwächter wurde beim Start beendet ({status})");
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn status() -> Result<WatchStatus> {
    if STATUS_CLOSING.load(std::sync::atomic::Ordering::Acquire) {
        bail!("Statuskanal wird beendet");
    }
    let config = configuration::load();
    let key = format!("{}\n{}", config.distro, config.watcher_path);
    let mut command = wsl();
    command.arg("/usr/bin/env")
        .arg(format!("HERDR_STATUS_WINDOWS_BOOT_TICKS={}", windows_boot_ticks()?))
        .arg("/usr/bin/python3").arg(&config.watcher_path).arg("--status-channel");
    let output = status_transport().lock().unwrap().request(key, command)?;
    let payload: StatusPayload = serde_json::from_value(output)
        .map_err(|error| anyhow!("Herdr-Status konnte nicht gelesen werden: {error}"))?;
    let agents = AgentSummary {
        available: payload.live_agents.available,
        total: payload.live_agents.total,
        working: payload.live_agents.working,
        idle: payload.live_agents.idle,
        done: payload.live_agents.done,
        blocked: payload.live_agents.blocked,
        unknown: payload.live_agents.unknown,
    };
    let preferred_completion_action =
        CompletionAction::from_wire(&payload.preferred_completion_action);
    let preferred_warning_seconds = if payload.preferred_warning_seconds == 0 {
        300
    } else {
        payload.preferred_warning_seconds
    };
    let state = match payload.state {
        None => {
            return Ok(WatchStatus::Off {
                agents,
                completion_action: preferred_completion_action,
                warning_seconds: preferred_warning_seconds,
            });
        }
        Some(state) => state,
    };
    let completion_action = CompletionAction::from_wire(&state.completion_action);
    let targets = state.targets.len();
    let live_scope = state.monitoring_scope == "live_agents";
    let _pane_ids: Vec<&str> = state
        .targets
        .iter()
        .map(|target| target.pane_id.as_str())
        .collect();
    if let Some(outcome) = state.outcome {
        return Ok(WatchStatus::Finished {
            outcome,
            agents,
            // A finished run retains its historical values in active-run.json.
            // The editable Live-Status controls must instead show the values for
            // the next run, which are the persisted preferences.
            completion_action: preferred_completion_action,
            warning_seconds: preferred_warning_seconds,
        });
    }
    if let Some(warning) = payload.shutdown_warning {
        let _warning_until = warning.cancelable_until;
        return Ok(WatchStatus::ShutdownWarning {
            warning_id: warning.warning_id,
            targets,
            live_scope,
            demo: state.demo,
            observe_only: state.dry_run,
            warning_seconds: state.warning_seconds,
            seconds_remaining: warning.seconds_remaining,
            agents,
            completion_action,
            network_triggered: warning.reason == "network_unavailable",
        });
    }
    Ok(WatchStatus::Watching {
        targets,
        live_scope,
        observe_only: state.dry_run,
        quiet: state.all_terminal_since.is_some(),
        demo: state.demo,
        agents,
        completion_action,
        warning_seconds: state.warning_seconds,
    })
}

pub fn start(observe_only: bool) -> Result<()> {
    let mut command = wsl();
    command.arg("/usr/bin/python3").arg(watcher_path()).args([
        "--arm",
        "--quiet-seconds",
        "5",
        "--poll-seconds",
        "1",
    ]);
    if observe_only {
        command.arg("--dry-run");
    }
    command_output(command, "Herdr night watch")?;
    if let Err(error) = spawn_watcher() {
        let _ = stop("scheduled_task_start_failed");
        return Err(error);
    }
    Ok(())
}

pub fn demo() -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--demo");
    command_output(command, "Herdr night watch demo")?;
    if let Err(error) = spawn_watcher() {
        let _ = stop("demo_start_failed");
        return Err(error);
    }
    Ok(())
}

pub fn stop(source: &str) -> Result<()> {
    let mut command = wsl();
    command.arg("/usr/bin/python3").arg(watcher_path()).args([
        "--cancel",
        "--cancel-source",
        source,
    ]);
    command_output(command, "Herdr night watch")?;
    Ok(())
}

pub fn confirm_completion(warning_id: &str) -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--confirm")
        .arg("--expected-warning-id")
        .arg(warning_id);
    command_output(command, "Sofortiger Abschluss")?;
    Ok(())
}

pub fn warning_is_current(expected: &str) -> Result<bool> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--warning-status");
    let output = command_output(command, "Warnungsprüfung")?;
    #[derive(Deserialize)]
    struct WarningIdentity {
        warning_id: Option<String>,
    }
    let identity: WarningIdentity = serde_json::from_str(&output)?;
    Ok(!expected.is_empty() && identity.warning_id.as_deref() == Some(expected))
}

pub fn cancel_warning(warning_id: &str) -> Result<()> {
    let mut command = wsl();
    command.arg("/usr/bin/python3").arg(watcher_path()).args([
        "--cancel",
        "--cancel-source",
        "warning_dialog",
        "--expected-warning-id",
        warning_id,
    ]);
    command_output(command, "Abschaltwarnung abbrechen")?;
    Ok(())
}

pub fn set_completion_action(action: CompletionAction) -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--set-completion-action")
        .arg(action.as_wire());
    command_output(command, "Abschlussmodus")?;
    Ok(())
}

pub fn set_warning_seconds(seconds: u64) -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--set-warning-seconds")
        .arg(seconds.to_string());
    command_output(command, "Warnfrist")?;
    Ok(())
}

pub fn completion_action(status: &WatchStatus) -> CompletionAction {
    match status {
        WatchStatus::Off {
            completion_action, ..
        }
        | WatchStatus::Watching {
            completion_action, ..
        }
        | WatchStatus::ShutdownWarning {
            completion_action, ..
        }
        | WatchStatus::Finished {
            completion_action, ..
        } => *completion_action,
    }
}

pub fn warning_seconds(status: &WatchStatus) -> u64 {
    match status {
        WatchStatus::Off {
            warning_seconds, ..
        }
        | WatchStatus::Watching {
            warning_seconds, ..
        }
        | WatchStatus::ShutdownWarning {
            warning_seconds, ..
        }
        | WatchStatus::Finished {
            warning_seconds, ..
        } => *warning_seconds,
    }
}

pub fn open_log() -> Result<()> {
    let home = {
        let mut command = wsl();
        command.arg("/usr/bin/printenv").arg("HOME");
        command_output(command, "WSL-Heimatverzeichnis")?
    };
    let home = home.trim();
    if !home.starts_with('/') {
        bail!("WSL-Heimatverzeichnis ist ungültig: {home}");
    }
    let home = home.trim_start_matches('/').replace('/', "\\");
    let path = format!(
        r"\\wsl.localhost\{}\{}\.local\state\herdr-night-watch\watch.log",
        configuration::load().distro,
        home,
    );
    Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .context("Protokoll konnte nicht geöffnet werden")?;
    Ok(())
}

static STATUS_INPUT: std::sync::OnceLock<crate::status_channel::Input> = std::sync::OnceLock::new();
static STATUS_TRANSPORT: std::sync::OnceLock<std::sync::Mutex<crate::status_channel::Channel>> = std::sync::OnceLock::new();
static STATUS_CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn status_transport() -> &'static std::sync::Mutex<crate::status_channel::Channel> {
    STATUS_TRANSPORT.get_or_init(|| {
        let input = STATUS_INPUT.get_or_init(Default::default).clone();
        std::sync::Mutex::new(crate::status_channel::Channel::new(input))
    })
}

pub fn shutdown_status_channel() {
    STATUS_CLOSING.store(true, std::sync::atomic::Ordering::Release);
    {
        let mut input = STATUS_INPUT.get_or_init(Default::default).lock().unwrap();
        input.closing = true;
        input.writer.take();
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    if let Some(channel) = STATUS_TRANSPORT.get() {
        while Instant::now() < deadline {
            if let Ok(mut channel) = channel.try_lock() { channel.close_until(deadline); break; }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

// Native boot timestamp, the same FILETIME value exposed by Windows CIM.
// No PowerShell/console process is needed for display-only status requests.
fn windows_boot_ticks() -> Result<i64> {
    #[repr(C)]
    #[derive(Default)]
    struct TimeOfDay { boot: i64, current: i64, zone_bias: i64, zone_id: u32,
        reserved: u32, boot_bias: u64, sleep_bias: u64 }
    #[link(name = "ntdll")]
    unsafe extern "system" { fn NtQuerySystemInformation(class: u32, data: *mut TimeOfDay,
        length: u32, returned: *mut u32) -> i32; }
    let mut data = TimeOfDay::default();
    let status = unsafe { NtQuerySystemInformation(3, &mut data,
        std::mem::size_of::<TimeOfDay>() as u32, std::ptr::null_mut()) };
    if status < 0 || data.boot <= 0 { bail!("Windows-Startzeit nicht verfügbar ({status})"); }
    Ok(data.boot)
}
