use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "HerdrNightWatchTray";

pub fn enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|key| key.get_value::<String, _>(VALUE_NAME))
        .is_ok()
}

pub fn set_enabled(on: bool) -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    if on {
        let exe = std::env::current_exe().context("Programmdatei konnte nicht bestimmt werden")?;
        let command = format!("\"{}\"", exe.display());
        let (key, _) = current_user
            .create_subkey(RUN_KEY)
            .context("Autostart konnte nicht eingerichtet werden")?;
        key.set_value(VALUE_NAME, &command)
            .context("Autostart konnte nicht eingerichtet werden")?;
    } else {
        let key = current_user
            .open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_WRITE)
            .context("Autostart konnte nicht entfernt werden")?;
        key.delete_value(VALUE_NAME)
            .context("Autostart konnte nicht entfernt werden")?;
    }
    Ok(())
}
