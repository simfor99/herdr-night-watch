use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::env;
use std::os::windows::process::CommandExt;
use std::process::Command;

const DEFAULT_DISTRO: &str = "Ubuntu";
const DEFAULT_CODEX_HOME: &str = "/home/user/.codex";
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
    cancelable_until: String,
    #[serde(default)]
    seconds_remaining: u64,
    #[serde(default)]
    reason: String,
}

fn distro() -> String {
    env::var("HERDR_WSL_DISTRO").unwrap_or_else(|_| DEFAULT_DISTRO.to_string())
}

fn codex_home() -> String {
    env::var("HERDR_CODEX_HOME").unwrap_or_else(|_| DEFAULT_CODEX_HOME.to_string())
}

fn watcher_path() -> String {
    format!("{}/bin/herdr-night-watch.py", codex_home())
}

fn windows_script_path(script: &str) -> String {
    let path = codex_home().trim_start_matches('/').replace('/', "\\");
    format!(r"\\wsl.localhost\{}\{}\windows\{}", distro(), path, script)
}

fn wsl() -> Command {
    let mut command = Command::new("wsl.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(distro())
        .arg("--exec");
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

pub fn status() -> Result<WatchStatus> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--status");
    let output = command_output(command, "Herdr-Statusabfrage")?;
    let payload: StatusPayload = serde_json::from_str(&output)
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

fn run_powershell(script: &str, dry_run: bool) -> Result<()> {
    let mut command = Command::new("powershell.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(windows_script_path(script));
    if dry_run {
        command.arg("-DryRun");
    }
    command_output(command, "Herdr-Nachtwächter")?;
    Ok(())
}

pub fn start(observe_only: bool) -> Result<()> {
    run_powershell("Start-HerdrNightWatch.ps1", observe_only)
}

pub fn demo() -> Result<()> {
    let mut command = Command::new("powershell.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(windows_script_path("Start-HerdrNightWatch.ps1"))
        .arg("-Demo");
    command_output(command, "Herdr-Nachtwächter-Demo")?;
    Ok(())
}

pub fn stop(source: &str) -> Result<()> {
    let mut command = Command::new("powershell.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(windows_script_path("Stop-HerdrNightWatch.ps1"))
        .arg("-CancelSource")
        .arg(source);
    command_output(command, "Herdr-Nachtwächter")?;
    Ok(())
}

pub fn confirm_completion() -> Result<()> {
    let mut command = wsl();
    command
        .arg("/usr/bin/python3")
        .arg(watcher_path())
        .arg("--confirm");
    command_output(command, "Sofortiger Abschluss")?;
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
    let path = format!(
        r"\\wsl.localhost\{}\home\user\.local\state\herdr-night-watch\watch.log",
        distro()
    );
    Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .context("Protokoll konnte nicht geöffnet werden")?;
    Ok(())
}
