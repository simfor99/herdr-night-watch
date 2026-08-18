use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const KEY: &str = r"Software\HerdrNachtwaechter";
const OPACITY_VALUE: &str = "WindowOpacity";
const LEVEL_VALUE: &str = "WindowLevel";
const LIVE_STATUS_START_VALUE: &str = "OpenLiveStatusOnStartup";
const LIVE_STATUS_TASKBAR_VALUE: &str = "ShowLiveStatusInTaskbar";
const LIVE_STATUS_POS_X_VALUE: &str = "LiveStatusPositionX";
const LIVE_STATUS_POS_Y_VALUE: &str = "LiveStatusPositionY";
const LIVE_STATUS_SCALE_VALUE: &str = "LiveStatusScale";

pub const OPACITY_VALUES: [u8; 10] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10];
pub const DEFAULT_LIVE_STATUS_SCALE: f32 = 1.0;
pub const MIN_LIVE_STATUS_SCALE: f32 = 0.75;
// This is a safety stop for corrupted settings or an accidental runaway
// drag, not a normal user-facing size limit. Windows constrains the practical
// size further through the available monitor work area.
pub const MAX_LIVE_STATUS_SCALE: f32 = 10.0;

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

pub fn live_status_in_taskbar() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(LIVE_STATUS_TASKBAR_VALUE))
        .map(|value| value != 0)
        .unwrap_or(true)
}

pub fn set_live_status_in_taskbar(show: bool) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(LIVE_STATUS_TASKBAR_VALUE, &u32::from(show))?;
    Ok(())
}

pub fn live_status_position() -> Option<[f32; 2]> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY).ok()?;
    let x = key.get_value::<u32, _>(LIVE_STATUS_POS_X_VALUE).ok()? as i32;
    let y = key.get_value::<u32, _>(LIVE_STATUS_POS_Y_VALUE).ok()? as i32;
    Some([x as f32, y as f32])
}

pub fn set_live_status_position(position: [f32; 2]) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    let x = position[0].round() as i32;
    let y = position[1].round() as i32;
    key.set_value(LIVE_STATUS_POS_X_VALUE, &(x as u32))?;
    key.set_value(LIVE_STATUS_POS_Y_VALUE, &(y as u32))?;
    Ok(())
}

pub fn live_status_scale() -> f32 {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(LIVE_STATUS_SCALE_VALUE))
        .ok()
        .map(|value| clamp_live_status_scale(value as f32 / 100.0))
        .unwrap_or(DEFAULT_LIVE_STATUS_SCALE)
}

pub fn set_live_status_scale(scale: f32) -> anyhow::Result<()> {
    let scale = clamp_live_status_scale(scale);
    let stored = (scale * 100.0).round() as u32;
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(LIVE_STATUS_SCALE_VALUE, &stored)?;
    Ok(())
}

pub fn reset_live_status_scale() -> anyhow::Result<()> {
    set_live_status_scale(DEFAULT_LIVE_STATUS_SCALE)
}

pub fn clamp_live_status_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(MIN_LIVE_STATUS_SCALE, MAX_LIVE_STATUS_SCALE)
    } else {
        DEFAULT_LIVE_STATUS_SCALE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_status_scale_is_bounded() {
        assert_eq!(clamp_live_status_scale(0.1), MIN_LIVE_STATUS_SCALE);
        assert_eq!(clamp_live_status_scale(12.0), MAX_LIVE_STATUS_SCALE);
        assert_eq!(clamp_live_status_scale(f32::NAN), DEFAULT_LIVE_STATUS_SCALE);
    }

    #[test]
    fn live_status_scale_preserves_fractional_percent() {
        assert!((clamp_live_status_scale(1.37) - 1.37).abs() < f32::EPSILON);
    }
}
