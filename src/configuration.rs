use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const KEY: &str = r"Software\HerdrNachtwaechter";
const DEFAULT_DISTRO: &str = "Ubuntu";
const DEFAULT_WATCHER_PATH: &str = "/home/user/.codex/bin/herdr-night-watch.py";

#[derive(Clone, Debug)]
pub struct Configuration {
    pub distro: String,
    pub watcher_path: String,
}

fn value(name: &str) -> Option<String> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value(name))
        .ok()
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
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(KEY)
        .context("configuration could not be saved")?;
    for (name, value) in [
        ("Distro", &configuration.distro),
        ("WatcherPath", &configuration.watcher_path),
    ] {
        key.set_value(name, value)
            .context("configuration could not be saved")?;
    }
    Ok(())
}
