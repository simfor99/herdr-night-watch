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
const LIVE_STATUS_QUOTA_OPEN_VALUE: &str = "LiveStatusQuotaOpen";
const LIVE_STATUS_QUOTA_DOCKED_VALUE: &str = "LiveStatusQuotaDocked";
const LIVE_STATUS_QUOTA_POS_X_VALUE: &str = "LiveStatusQuotaPositionX";
const LIVE_STATUS_QUOTA_POS_Y_VALUE: &str = "LiveStatusQuotaPositionY";
const LIVE_STATUS_QUOTA_SHOW_GLM_VALUE: &str = "LiveStatusQuotaShowGlm";
const LIVE_STATUS_QUOTA_SHOW_AGI_VALUE: &str = "LiveStatusQuotaShowAgi";
const LIVE_STATUS_QUOTA_SHOW_CODEX_VALUE: &str = "LiveStatusQuotaShowCodex";
const LIVE_STATUS_QUOTA_SHOW_CLAUDE_VALUE: &str = "LiveStatusQuotaShowClaude";
const LIVE_STATUS_QUOTA_COLOR_GLM_VALUE: &str = "LiveStatusQuotaColorGlm";
const LIVE_STATUS_QUOTA_COLOR_AGI_VALUE: &str = "LiveStatusQuotaColorAgi";
const LIVE_STATUS_QUOTA_COLOR_CODEX_VALUE: &str = "LiveStatusQuotaColorCodex";
const LIVE_STATUS_QUOTA_COLOR_CLAUDE_VALUE: &str = "LiveStatusQuotaColorClaude";
const LIVE_STATUS_CORNER_RADIUS_VALUE: &str = "LiveStatusCornerRadius";
const LIVE_STATUS_QUOTA_SETTINGS_OPEN_VALUE: &str = "LiveStatusQuotaSettingsOpen";

pub const DEFAULT_LIVE_STATUS_CORNER_RADIUS: u8 = 10;
pub const MIN_LIVE_STATUS_CORNER_RADIUS: u8 = 0;
pub const MAX_LIVE_STATUS_CORNER_RADIUS: u8 = 20;
pub const CORNER_RADIUS_PRESETS: [u8; 6] = [0, 4, 8, 10, 14, 18];

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

pub fn live_status_quota_open() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_OPEN_VALUE, false)
}

pub fn set_live_status_quota_open(open: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_OPEN_VALUE, open)
}

pub fn live_status_quota_settings_open() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_SETTINGS_OPEN_VALUE, false)
}

pub fn set_live_status_quota_settings_open(open: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_SETTINGS_OPEN_VALUE, open)
}

pub fn live_status_quota_docked() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_DOCKED_VALUE, true)
}

pub fn set_live_status_quota_docked(docked: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_DOCKED_VALUE, docked)
}

pub fn live_status_quota_position() -> Option<[f32; 2]> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY).ok()?;
    let x = key.get_value::<u32, _>(LIVE_STATUS_QUOTA_POS_X_VALUE).ok()? as i32;
    let y = key.get_value::<u32, _>(LIVE_STATUS_QUOTA_POS_Y_VALUE).ok()? as i32;
    Some([x as f32, y as f32])
}

pub fn set_live_status_quota_position(position: [f32; 2]) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    let x = position[0].round() as i32;
    let y = position[1].round() as i32;
    key.set_value(LIVE_STATUS_QUOTA_POS_X_VALUE, &(x as u32))?;
    key.set_value(LIVE_STATUS_QUOTA_POS_Y_VALUE, &(y as u32))?;
    Ok(())
}

pub fn live_status_quota_show_glm() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_SHOW_GLM_VALUE, true)
}

pub fn set_live_status_quota_show_glm(show: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_SHOW_GLM_VALUE, show)
}

pub fn live_status_quota_show_agi() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_SHOW_AGI_VALUE, true)
}

pub fn set_live_status_quota_show_agi(show: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_SHOW_AGI_VALUE, show)
}

pub fn live_status_quota_show_codex() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_SHOW_CODEX_VALUE, true)
}

pub fn set_live_status_quota_show_codex(show: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_SHOW_CODEX_VALUE, show)
}

pub fn live_status_quota_show_claude() -> bool {
    read_bool_setting(LIVE_STATUS_QUOTA_SHOW_CLAUDE_VALUE, false)
}

pub fn set_live_status_quota_show_claude(show: bool) -> anyhow::Result<()> {
    write_bool_setting(LIVE_STATUS_QUOTA_SHOW_CLAUDE_VALUE, show)
}

fn read_u32_setting(value_name: &str, default: u32) -> u32 {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|key| key.get_value::<u32, _>(value_name))
        .unwrap_or(default)
}

fn write_u32_setting(value_name: &str, value: u32) -> anyhow::Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    key.set_value(value_name, &value)?;
    Ok(())
}

pub fn live_status_quota_color_glm() -> u8 {
    read_u32_setting(LIVE_STATUS_QUOTA_COLOR_GLM_VALUE, 6).min(9) as u8
}

pub fn set_live_status_quota_color_glm(idx: u8) -> anyhow::Result<()> {
    write_u32_setting(LIVE_STATUS_QUOTA_COLOR_GLM_VALUE, idx.min(9) as u32)
}

pub fn live_status_quota_color_agi() -> u8 {
    read_u32_setting(LIVE_STATUS_QUOTA_COLOR_AGI_VALUE, 0).min(9) as u8
}

pub fn set_live_status_quota_color_agi(idx: u8) -> anyhow::Result<()> {
    write_u32_setting(LIVE_STATUS_QUOTA_COLOR_AGI_VALUE, idx.min(9) as u32)
}

pub fn live_status_quota_color_codex() -> u8 {
    read_u32_setting(LIVE_STATUS_QUOTA_COLOR_CODEX_VALUE, 3).min(9) as u8
}

pub fn set_live_status_quota_color_codex(idx: u8) -> anyhow::Result<()> {
    write_u32_setting(LIVE_STATUS_QUOTA_COLOR_CODEX_VALUE, idx.min(9) as u32)
}

pub fn live_status_quota_color_claude() -> u8 {
    read_u32_setting(LIVE_STATUS_QUOTA_COLOR_CLAUDE_VALUE, 6).min(9) as u8
}

pub fn set_live_status_quota_color_claude(idx: u8) -> anyhow::Result<()> {
    write_u32_setting(LIVE_STATUS_QUOTA_COLOR_CLAUDE_VALUE, idx.min(9) as u32)
}

pub fn clamp_corner_radius(radius: u8) -> u8 {
    radius.clamp(MIN_LIVE_STATUS_CORNER_RADIUS, MAX_LIVE_STATUS_CORNER_RADIUS)
}

pub fn live_status_corner_radius() -> u8 {
    read_u32_setting(
        LIVE_STATUS_CORNER_RADIUS_VALUE,
        u32::from(DEFAULT_LIVE_STATUS_CORNER_RADIUS),
    )
    .min(u32::from(MAX_LIVE_STATUS_CORNER_RADIUS)) as u8
}

pub fn set_live_status_corner_radius(radius: u8) -> anyhow::Result<()> {
    write_u32_setting(
        LIVE_STATUS_CORNER_RADIUS_VALUE,
        u32::from(clamp_corner_radius(radius)),
    )
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

    #[test]
    fn quota_provider_visibility_constants_are_distinct() {
        let keys = [
            LIVE_STATUS_QUOTA_SHOW_GLM_VALUE,
            LIVE_STATUS_QUOTA_SHOW_AGI_VALUE,
            LIVE_STATUS_QUOTA_SHOW_CODEX_VALUE,
            LIVE_STATUS_QUOTA_SHOW_CLAUDE_VALUE,
        ];
        for (i, a) in keys.iter().enumerate() {
            for (j, b) in keys.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn quota_provider_color_constants_are_distinct() {
        let keys = [
            LIVE_STATUS_QUOTA_COLOR_GLM_VALUE,
            LIVE_STATUS_QUOTA_COLOR_AGI_VALUE,
            LIVE_STATUS_QUOTA_COLOR_CODEX_VALUE,
            LIVE_STATUS_QUOTA_COLOR_CLAUDE_VALUE,
        ];
        for (i, a) in keys.iter().enumerate() {
            for (j, b) in keys.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn corner_radius_clamps_properly() {
        assert_eq!(clamp_corner_radius(0), 0);
        assert_eq!(clamp_corner_radius(10), 10);
        assert_eq!(clamp_corner_radius(20), 20);
        assert_eq!(clamp_corner_radius(25), 20);
        assert_eq!(DEFAULT_LIVE_STATUS_CORNER_RADIUS, 10);
    }
}
