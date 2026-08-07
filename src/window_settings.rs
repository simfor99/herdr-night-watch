use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const KEY: &str = r"Software\HerdrNachtwaechter";
const OPACITY_VALUE: &str = "WindowOpacity";
const LEVEL_VALUE: &str = "WindowLevel";
const LIVE_STATUS_START_VALUE: &str = "OpenLiveStatusOnStartup";

pub const OPACITY_VALUES: [u8; 10] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowLevel {
    Normal,
    AlwaysOnTop,
    AlwaysOnBottom,
}

impl WindowLevel {
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::AlwaysOnTop,
            Self::AlwaysOnTop => Self::AlwaysOnBottom,
            Self::AlwaysOnBottom => Self::Normal,
        }
    }

    pub fn current() -> Self {
        let value = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(KEY)
            .and_then(|key| key.get_value::<String, _>(LEVEL_VALUE))
            .unwrap_or_default();
        match value.as_str() {
            "top" => Self::AlwaysOnTop,
            "bottom" => Self::AlwaysOnBottom,
            _ => Self::Normal,
        }
    }

    pub fn set(self) -> anyhow::Result<()> {
        let value = match self {
            Self::Normal => "normal",
            Self::AlwaysOnTop => "top",
            Self::AlwaysOnBottom => "bottom",
        };
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
        key.set_value(LEVEL_VALUE, &value)?;
        Ok(())
    }
}

pub fn opacity() -> u8 {
    let value = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(OPACITY_VALUE))
        .ok()
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(100);
    if OPACITY_VALUES.contains(&value) {
        value
    } else {
        100
    }
}

pub fn set_opacity(value: u8) -> anyhow::Result<()> {
    let value = if OPACITY_VALUES.contains(&value) {
        value
    } else {
        100
    };
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(OPACITY_VALUE, &u32::from(value))?;
    Ok(())
}

pub fn live_status_on_start() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(LIVE_STATUS_START_VALUE))
        .map(|value| value != 0)
        .unwrap_or(false)
}

pub fn set_live_status_on_start(enabled: bool) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(LIVE_STATUS_START_VALUE, &u32::from(enabled))?;
    Ok(())
}
