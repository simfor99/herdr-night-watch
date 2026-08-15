use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const HISTORY_LIMIT: usize = 30;
const HISTORY_HEADER: &str = "Datum und Uhrzeit;Aktion;Auslöser;Lauf-ID";
const UNEXPECTED_EXIT_ACTION: &str = "Tray-App unplanmäßig beendet";
const UNEXPECTED_EXIT_TRIGGER: &str = "Vorherige Sitzung ohne sauberes Ende";

#[derive(Debug, Deserialize, Serialize)]
struct SessionMarker {
    session_id: String,
    started_at: String,
    expected_exit: bool,
    #[serde(default)]
    expected_history_tail: Option<String>,
}

pub fn start_session() {
    let Ok(directory) = log_directory() else {
        return;
    };
    let _ = fs::create_dir_all(&directory);
    let marker_path = directory.join("tray-session.json");

    if let Ok(previous) = read_marker(&marker_path) {
        let expected_action_recorded = previous.expected_exit
            && previous.expected_history_tail.as_deref() != history_tail(&directory).as_deref();
        if !expected_action_recorded {
            let _ = append_event(
                &directory,
                &local_timestamp(),
                UNEXPECTED_EXIT_ACTION,
                UNEXPECTED_EXIT_TRIGGER,
                &previous.session_id,
            );
        }
    }

    let marker = SessionMarker {
        session_id: format!("tray-{}-{}", compact_timestamp(), process::id()),
        started_at: local_timestamp(),
        expected_exit: false,
        expected_history_tail: None,
    };
    let _ = write_marker(&marker_path, &marker);
}

pub fn set_expected_exit(expected: bool) {
    let Ok(directory) = log_directory() else {
        return;
    };
    let marker_path = directory.join("tray-session.json");
    let Ok(mut marker) = read_marker(&marker_path) else {
        return;
    };
    marker.expected_exit = expected;
    marker.expected_history_tail = if expected {
        history_tail(&directory)
    } else {
        None
    };
    let _ = write_marker(&marker_path, &marker);
}

pub fn finish_session() {
    let Ok(directory) = log_directory() else {
        return;
    };
    let _ = fs::remove_file(directory.join("tray-session.json"));
}

fn log_directory() -> std::io::Result<PathBuf> {
    std::env::current_exe()?
        .parent()
        .map(|directory| directory.join("logs"))
        .ok_or_else(|| std::io::Error::other("Installationsordner fehlt"))
}

fn read_marker(path: &Path) -> std::io::Result<SessionMarker> {
    let contents = fs::read_to_string(path)?;
    serde_json::from_str(&contents)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn write_marker(path: &Path, marker: &SessionMarker) -> std::io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::to_string_pretty(marker).map_err(std::io::Error::other)?;
    fs::write(&temporary, format!("{contents}\n"))?;
    replace_file(&temporary, path)
}

fn append_event(
    directory: &Path,
    timestamp: &str,
    action: &str,
    trigger: &str,
    session_id: &str,
) -> std::io::Result<()> {
    let path = directory.join("completion-history.csv");
    let mut rows: Vec<String> = if path.exists() {
        fs::read_to_string(&path)?
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    if rows.len() >= HISTORY_LIMIT {
        rows.drain(0..rows.len() - (HISTORY_LIMIT - 1));
    }
    rows.push(format!(
        "{};{};{};{}",
        csv_safe(timestamp),
        csv_safe(action),
        csv_safe(trigger),
        csv_safe(session_id),
    ));

    fs::create_dir_all(directory)?;
    let temporary = path.with_extension("csv.tray.tmp");
    let mut contents = String::from(HISTORY_HEADER);
    contents.push('\n');
    if !rows.is_empty() {
        contents.push_str(&rows.join("\n"));
        contents.push('\n');
    }
    fs::write(&temporary, contents)?;
    replace_file(&temporary, &path)
}

fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(first_error) if destination.exists() => {
            let backup = destination.with_extension("bak");
            let _ = fs::remove_file(&backup);
            fs::rename(destination, &backup)?;
            match fs::rename(temporary, destination) {
                Ok(()) => {
                    let _ = fs::remove_file(backup);
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&backup, destination);
                    Err(error)
                }
            }
        }
        Err(error) => Err(error),
    }
}

fn history_tail(directory: &Path) -> Option<String> {
    let completion = read_tail(&directory.join("completion-history.csv"));
    let cancellation = read_tail(&directory.join("cancellation-history.csv"));
    if completion.is_none() && cancellation.is_none() {
        return None;
    }
    Some(format!(
        "completion={};cancellation={}",
        completion.unwrap_or_default(),
        cancellation.unwrap_or_default()
    ))
}

fn read_tail(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().and_then(|contents| {
        contents
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::to_owned)
    })
}

fn csv_safe(value: &str) -> String {
    value.replace(['\r', '\n', ';'], " ")
}

fn local_timestamp() -> String {
    let mut system_time = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { GetLocalTime(&mut system_time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        system_time.wYear,
        system_time.wMonth,
        system_time.wDay,
        system_time.wHour,
        system_time.wMinute,
        system_time.wSecond
    )
}

fn compact_timestamp() -> String {
    local_timestamp().replace(['-', ' ', ':'], "")
}
