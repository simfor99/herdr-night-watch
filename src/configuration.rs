use anyhow::{Context, Result};
use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const KEY: &str = r"HKCU\Software\HerdrNachtwaechter";
const DEFAULT_DISTRO: &str = "Ubuntu";
const DEFAULT_WATCHER_PATH: &str = "/home/user/.codex/bin/herdr-night-watch.py";

#[derive(Clone, Debug)]
pub struct Configuration {
    pub distro: String,
    pub watcher_path: String,
}

fn value(name: &str) -> Option<String> {
    let output = Command::new("reg.exe")
        .args(["query", KEY, "/v", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 3 && fields[0].eq_ignore_ascii_case(name) {
                Some(fields[2..].join(" "))
            } else {
                None
            }
        })
}

pub fn load() -> Configuration {
    let distro = value("Distro")
        .or_else(|| std::env::var("HERDR_WSL_DISTRO").ok())
        .unwrap_or_else(|| DEFAULT_DISTRO.into());
    let watcher_path = value("WatcherPath")
        .or_else(|| std::env::var("HERDR_WATCHER_PATH").ok())
        .or_else(|| {
            std::env::var("HERDR_CODEX_HOME")
                .ok()
                .map(|home| format!("{home}/bin/herdr-night-watch.py"))
        })
        .unwrap_or_else(|| DEFAULT_WATCHER_PATH.into());
    Configuration {
        distro,
        watcher_path,
    }
}

pub fn save(configuration: &Configuration) -> Result<()> {
    for (name, value) in [
        ("Distro", &configuration.distro),
        ("WatcherPath", &configuration.watcher_path),
    ] {
        let status = Command::new("reg.exe")
            .args(["add", KEY, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .context("configuration could not be saved")?;
        anyhow::ensure!(status.success(), "configuration could not be saved");
    }
    Ok(())
}
