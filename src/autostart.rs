use anyhow::{Context, Result};
use std::os::windows::process::CommandExt;
use std::process::Command;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "HerdrNightWatchTray";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn enabled() -> bool {
    let mut command = Command::new("reg.exe");
    command
        .creation_flags(CREATE_NO_WINDOW)
        .args(["query", RUN_KEY, "/v", VALUE_NAME]);
    command.output().is_ok_and(|output| output.status.success())
}

pub fn set_enabled(on: bool) -> Result<()> {
    if on {
        let exe = std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
        let command = format!("\"{}\"", exe.display());
        let mut registry = Command::new("reg.exe");
        registry.creation_flags(CREATE_NO_WINDOW).args([
            "add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", &command, "/f",
        ]);
        let status = registry
            .status()
            .context("Autostart konnte nicht eingerichtet werden")?;
        if !status.success() {
            anyhow::bail!("Autostart konnte nicht eingerichtet werden")
        }
    } else {
        let mut registry = Command::new("reg.exe");
        registry
            .creation_flags(CREATE_NO_WINDOW)
            .args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]);
        let status = registry
            .status()
            .context("Autostart konnte nicht entfernt werden")?;
        if !status.success() {
            anyhow::bail!("Autostart konnte nicht entfernt werden")
        }
    }
    Ok(())
}
