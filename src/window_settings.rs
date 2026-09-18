use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const KEY: &str = r"Software\HerdrNachtwaechter";
const OPACITY_VALUE: &str = "WindowOpacity";
const LEVEL_VALUE: &str = "WindowLevel";
const LIVE_STATUS_START_VALUE: &str = "OpenLiveStatusOnStartup";
const LIVE_STATUS_TASKBAR_VALUE: &str = "ShowLiveStatusInTaskbar";
const CLOCK_VISIBLE_VALUE: &str = "ShowAnalogClock";
const CLOCK_SECOND_HAND_VALUE: &str = "ShowClockSecondHand";
const LIVE_STATUS_POS_X_VALUE: &str = "LiveStatusPositionX";
const LIVE_STATUS_POS_Y_VALUE: &str = "LiveStatusPositionY";
const LIVE_STATUS_SCALE_VALUE: &str = "LiveStatusScale";
const LIVE_STATUS_REPAINT_VALUE: &str = "LiveStatusRepaintIntervalMs";

pub const OPACITY_VALUES: [u8; 10] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10];
// The live-status repaint cadence trades the gliding second hand against
// the OpenGL black-flash risk: every present has a small random chance to
// drop the frame, so fewer repaints mean fewer flashes.
pub const REPAINT_INTERVAL_MS_VALUES: [u32; 3] = [250, 500, 1000];
pub const DEFAULT_REPAINT_INTERVAL_MS: u32 = 250;
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
    read_bool_setting(LIVE_STATUS_START_VALUE, true)
}

pub fn set_live_status_on_start(enabled: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_START_VALUE, enabled)
}

pub fn live_status_in_taskbar() -> bool {
    read_bool_setting(LIVE_STATUS_TASKBAR_VALUE, true)
}

pub fn set_live_status_in_taskbar(show: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_TASKBAR_VALUE, show)
}

pub fn clock_visible() -> bool {
    read_bool_setting(CLOCK_VISIBLE_VALUE, true)
}

pub fn set_clock_visible(show: bool) -> anyhow::Result<()> {
    write_bool_setting(CLOCK_VISIBLE_VALUE, show)
}

fn bool_setting_value(value: Option<u32>, default: bool) -> bool {
    value.map(|value| value != 0).unwrap_or(default)
}

fn read_bool_setting(value_name: &str, default: bool) -> bool {
    let value = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(value_name))
        .ok();
    bool_setting_value(value, default)
}

fn write_bool_setting(value_name: &str, value: bool) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(value_name, &u32::from(value))?;
    Ok(())
}

pub fn clock_second_hand_visible() -> bool {
    read_bool_setting(CLOCK_SECOND_HAND_VALUE, true)
}

pub fn live_status_repaint_interval_ms() -> u32 {
    clamp_repaint_interval_ms(
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(KEY)
            .and_then(|key| key.get_value::<u32, _>(LIVE_STATUS_REPAINT_VALUE))
            .ok(),
    )
}

pub fn set_live_status_repaint_interval_ms(value: u32) -> anyhow::Result<()> {
    // Validate instead of clamping: the context menu only offers the three
    // supported intervals, so anything else is a caller bug worth surfacing.
    if !REPAINT_INTERVAL_MS_VALUES.contains(&value) {
        anyhow::bail!("unsupported repaint interval: {value} ms");
    }
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(LIVE_STATUS_REPAINT_VALUE, &value)?;
    Ok(())
}

fn clamp_repaint_interval_ms(value: Option<u32>) -> u32 {
    value
        .filter(|value| REPAINT_INTERVAL_MS_VALUES.contains(value))
        .unwrap_or(DEFAULT_REPAINT_INTERVAL_MS)
}

pub fn set_clock_second_hand_visible(show: bool) -> anyhow::Result<()> {
    write_bool_setting(CLOCK_SECOND_HAND_VALUE, show)
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

    #[test]
    fn bool_setting_value_uses_the_default_and_treats_zero_as_disabled() {
        assert!(bool_setting_value(None, true));
        assert!(!bool_setting_value(Some(0), true));
        assert!(bool_setting_value(Some(1), true));
    }

    #[test]
    fn repaint_interval_accepts_only_the_offered_choices() {
        assert_eq!(clamp_repaint_interval_ms(None), DEFAULT_REPAINT_INTERVAL_MS);
        assert_eq!(
            clamp_repaint_interval_ms(Some(0)),
            DEFAULT_REPAINT_INTERVAL_MS
        );
        assert_eq!(
            clamp_repaint_interval_ms(Some(333)),
            DEFAULT_REPAINT_INTERVAL_MS
        );
        assert_eq!(clamp_repaint_interval_ms(Some(250)), 250);
        assert_eq!(clamp_repaint_interval_ms(Some(500)), 500);
        assert_eq!(clamp_repaint_interval_ms(Some(1000)), 1000);
    }
}
