//! KI-Budget-Limits (GLM, Agy, Codex, Claude, Qwen) Telemetry & Management for Herdr Night Watch.
//!
//! Provides data structures, parsers, and background sampling for provider quotas.
//! Fail-closed: values are strictly informational and never participate in shutdown decisions.

use crate::language::Language;
use std::time::Instant;

#[cfg(windows)]
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Clone, Copy)]
struct WinTimeZoneInformation {
    Bias: i32,
    StandardName: [u16; 32],
    StandardDate: windows_sys::Win32::Foundation::SYSTEMTIME,
    StandardBias: i32,
    DaylightName: [u16; 32],
    DaylightDate: windows_sys::Win32::Foundation::SYSTEMTIME,
    DaylightBias: i32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SystemTimeToTzSpecificLocalTime(
        time_zone_information: *const WinTimeZoneInformation,
        universal_time: *const windows_sys::Win32::Foundation::SYSTEMTIME,
        local_time: *mut windows_sys::Win32::Foundation::SYSTEMTIME,
    ) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    Glm,
    Agy,
    Codex,
    Claude,
    Qwen,
}

/// Number of providers the settings tab can ever list. Keep in sync with
/// `ProviderId`; the satellite height derives from this so a newly added
/// provider never clips off-screen.
pub const PROVIDER_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexObservationSource {
    AppServer,
    SessionLog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeekPendingReason {
    EarlyWindowAge,
    LowEarlyUsage,
    ZeroUsage,
    InvalidOrMissing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeekForecastState {
    Active,
    ProvenDeficit,
    EstimateNeutral,
    Pending(WeekPendingReason),
    ConfirmedExhausted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderQuota {
    pub id: ProviderId,
    pub title: &'static str,
    pub author: &'static str,
    pub five_hour_percent: Option<u8>,
    pub five_hour_reset: Option<String>,
    pub week_percent: Option<u8>,
    pub week_reset: Option<String>,
    pub is_throttled: bool,
    /// True when the large window is a monthly (not weekly) cycle.
    pub month_cycle: bool,
    /// Exact cycle length in days when the live payload reports the window
    /// start; `None` when only the reset date is known.
    pub cycle_days: Option<f32>,
    /// Unrounded consumption of the large window (0-100) when the payload
    /// carries it; keeps the pace forecast free of integer-percent noise.
    pub week_consumed_pct: Option<f32>,
    /// Exact UTC reset epoch of the large window when reported by the fetcher.
    pub week_reset_epoch: Option<i64>,
    /// Exact UTC observation epoch of the large window when reported by the fetcher.
    pub week_observed_epoch: Option<i64>,
    /// Provenance of the Codex observation (`app_server` vs `session_log`).
    pub week_source: Option<CodexObservationSource>,
    /// Optional proven consumption uncertainty interval `(low_pct, high_pct)`.
    pub week_consumed_range: Option<(f32, f32)>,
    /// True when weekly exhaustion is unambiguously confirmed (not a rounded-zero log value).
    pub week_confirmed_exhausted: bool,
    /// True when 5-hour exhaustion is unambiguously confirmed.
    pub five_hour_confirmed_exhausted: bool,
    /// Exact UTC reset epoch of the 5-hour window when reported by the fetcher.
    pub five_hour_reset_epoch: Option<i64>,
    /// True when the provider reported an explicit lock without window attribution.
    pub explicit_provider_throttle: bool,
    /// Stable slot id of a multi-account provider card (`None` for single-account providers).
    pub account_id: Option<String>,
    /// Short account label such as "personal"; the card title shows it only when several account cards exist.
    pub account_label: Option<String>,
    /// Account e-mail, shown in the hover tooltip only.
    pub account_email: Option<String>,
    /// False for a registered account that the provider does not currently use.
    pub is_active_account: bool,
    /// Authentication problem reported by the fetcher (for example re-login required).
    pub auth_error: Option<String>,
    /// Earliest weekly refill of a healthy partner account, set only on the active account when it precedes that account's own weekly reset.
    pub pool_next_refill_epoch: Option<i64>,
}

impl ProviderQuota {
    pub fn new(
        id: ProviderId,
        title: &'static str,
        author: &'static str,
        five_hour_percent: Option<u8>,
        five_hour_reset: Option<String>,
        week_percent: Option<u8>,
        week_reset: Option<String>,
    ) -> Self {
        let five_hour_confirmed_exhausted = five_hour_percent == Some(0);
        let week_confirmed_exhausted = week_percent == Some(0);
        let is_throttled = five_hour_confirmed_exhausted || week_confirmed_exhausted;
        Self {
            id,
            title,
            author,
            five_hour_percent,
            five_hour_reset,
            week_percent,
            week_reset,
            is_throttled,
            // Z.ai and the QwenCloud Token Plan renew their large window
            // monthly; a live Qwen payload may correct this per cycle field.
            month_cycle: matches!(id, ProviderId::Glm | ProviderId::Qwen),
            cycle_days: None,
            week_consumed_pct: None,
            week_reset_epoch: None,
            week_observed_epoch: None,
            week_source: None,
            week_consumed_range: None,
            week_confirmed_exhausted,
            five_hour_confirmed_exhausted,
            five_hour_reset_epoch: None,
            explicit_provider_throttle: false,
            account_id: None,
            account_label: None,
            account_email: None,
            is_active_account: true,
            auth_error: None,
            pool_next_refill_epoch: None,
        }
    }

    #[allow(dead_code)]
    pub fn status_text(&self) -> &'static str {
        if self.is_throttled {
            "Drossel aktiv"
        } else if self.week_percent.unwrap_or(100) < 20 || self.five_hour_percent.unwrap_or(100) < 20 {
            "Knapp"
        } else {
            "Aktiv"
        }
    }

    pub fn cycle_label(&self) -> &'static str {
        if self.month_cycle || self.id == ProviderId::Glm {
            "Mo"
        } else if let Some(reset) = &self.week_reset {
            if reset.starts_with("01.") {
                "Mo"
            } else {
                "Wk"
            }
        } else {
            "Wk"
        }
    }

    #[allow(dead_code)]
    pub fn provider_is_throttled_at(&self, now_epoch: i64) -> bool {
        if self.id == ProviderId::Codex {
            let week_active = self.week_confirmed_exhausted
                && self.week_reset_epoch.is_none_or(|r| now_epoch < r);
            let five_h_active = self.five_hour_confirmed_exhausted
                && self.five_hour_reset_epoch.is_none_or(|r| now_epoch < r);
            return week_active || five_h_active || self.explicit_provider_throttle;
        }
        self.is_throttled
    }

    #[allow(dead_code)]
    pub fn provider_is_throttled(&self) -> bool {
        self.provider_is_throttled_at(current_utc_epoch())
    }

    #[allow(dead_code)]
    pub fn week_is_throttled(&self) -> bool {
        if self.id == ProviderId::Codex {
            return self.week_confirmed_exhausted
                && self.week_reset_epoch.is_none_or(|r| current_utc_epoch() < r);
        }
        self.is_throttled
    }

    #[allow(dead_code)]
    pub fn five_hour_is_throttled(&self) -> bool {
        if self.id == ProviderId::Codex {
            let five_h_active = self.five_hour_confirmed_exhausted
                && self.five_hour_reset_epoch.is_none_or(|r| current_utc_epoch() < r);
            return five_h_active || self.week_is_throttled();
        }
        self.is_throttled
    }

    fn five_hour_window_at(&self, now_epoch: i64) -> (Option<u8>, Option<&str>) {
        if self.id == ProviderId::Codex && self.five_hour_reset_epoch.is_some_and(|reset| now_epoch >= reset) {
            return (None, None);
        }
        (self.five_hour_percent, self.five_hour_reset.as_deref())
    }

    #[cfg(windows)]
    pub fn current_five_hour_window(&self) -> (Option<u8>, Option<&str>) {
        self.five_hour_window_at(current_utc_epoch())
    }

    pub fn pacing_forecast(&self) -> PacingForecast {
        if self.id == ProviderId::Codex {
            return self.codex_pacing_forecast_at(
                current_utc_epoch(),
                current_forecast_today(),
                current_hm(),
            );
        }
        let mut fc = pacing_forecast_with_cycle_days(
            self.is_throttled,
            self.week_percent,
            self.week_reset.as_deref(),
            self.five_hour_percent,
            self.five_hour_reset.as_deref(),
            self.cycle_days,
            self.week_consumed_pct,
            None,
            None,
        );
        apply_pool_refill_cap(&mut fc, self.pool_next_refill_epoch, current_utc_epoch());
        fc
    }

    /// Whether this card may raise the global throttle alarm. An inactive
    /// reserve account only does so when every account of its provider is
    /// throttled, so an exhausted spare never flags a healthy active account.
    pub fn multi_account_alarm_allowed(&self, providers: &[ProviderQuota]) -> bool {
        if self.account_id.is_none() || self.is_active_account {
            return true;
        }
        providers
            .iter()
            .filter(|p| p.id == self.id && p.account_id.is_some())
            .all(|p| p.provider_is_throttled())
    }

    pub fn codex_pacing_forecast_at(
        &self,
        now_epoch: i64,
        today: (i32, u32, u32),
        now_hm: (u32, u32),
    ) -> PacingForecast {
        let (five_hour_percent, five_hour_reset) = self.five_hour_window_at(now_epoch);
        let mut fc = codex_week_pacing_forecast(
            self.week_percent,
            self.week_reset.as_deref(),
            self.week_reset_epoch,
            self.week_observed_epoch,
            self.week_consumed_pct,
            self.week_consumed_range,
            self.week_source,
            self.week_confirmed_exhausted,
            five_hour_percent,
            five_hour_reset,
            now_epoch,
            today,
            now_hm,
        );
        if self.provider_is_throttled_at(now_epoch) {
            fc.health = PacingHealth::Throttled;
        }
        fc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacingHealth {
    Surplus,
    OnTrack,
    Tight,
    Throttled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FiveHourForecast {
    pub burn_rate: f32, // % per minute
    pub pace_ratio: f32,
    pub runway_minutes: Option<u32>,
    pub delta_minutes: Option<i32>,
    pub exhaustion_time: Option<(u32, u32)>, // (HH, MM)
    pub reset_time: (u32, u32),              // (HH, MM)
    pub is_exhausted_before_reset: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PacingForecast {
    pub pace_ratio: Option<f32>,
    pub runway_days: Option<f32>,
    pub delta_days: Option<i32>,
    pub remaining_days: f32,
    pub health: PacingHealth,
    pub badge_text: String,
    pub summary_text: String,
    pub five_hour_forecast: Option<FiveHourForecast>,
    pub week_exhaustion_date: Option<(u32, u32)>, // (month, day)
    pub reset_str: Option<String>,
    pub today: (i32, u32, u32),
    pub week_state: WeekForecastState,
    pub week_is_estimate: bool,
    projected_week_reserve: Option<f32>,
}

impl PacingForecast {
    #[allow(dead_code)]
    pub fn five_hour_is_deficit(&self) -> bool {
        if self.health == PacingHealth::Throttled {
            return true;
        }
        self.five_hour_forecast
            .as_ref()
            .map(|fh| fh.is_exhausted_before_reset)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn week_is_deficit(&self) -> bool {
        if self.health == PacingHealth::Throttled {
            return true;
        }
        self.delta_days.map(|d| d < 0).unwrap_or_else(|| {
            self.runway_days.unwrap_or(self.remaining_days) < self.remaining_days
        })
    }

    /// Protection shield in color form (parity with the health rule): with at
    /// least 50 % remaining and a pace of at most 1.25x, a small deficit is a
    /// warning, not a red alert. Keeps a 98 % monthly card from flashing red
    /// over "-2d" while the pace itself is near plan.
    pub fn week_deficit_shielded(&self, remaining_percent: u8) -> bool {
        remaining_percent >= 50 && self.pace_ratio.map_or(false, |ratio| ratio <= 1.25)
    }

    pub fn localized_upper_line(&self, language: Language) -> String {
        if self.health == PacingHealth::Throttled {
            return language.text("Gedrosselt", "Throttled").to_string();
        }

        if let Some(fh) = &self.five_hour_forecast {
            if fh.is_exhausted_before_reset {
                let (h, m) = fh.exhaustion_time.unwrap_or((0, 0));
                let def_mins = fh.delta_minutes.map(|d| (-d).max(1)).unwrap_or(30);
                if def_mins < 60 {
                    match language {
                        Language::German => format!("-{} Min. ({:02}:{:02})", def_mins, h, m),
                        Language::English => format!("-{} min ({:02}:{:02})", def_mins, h, m),
                    }
                } else {
                    let hours = (def_mins as f32 / 60.0).round() as i32;
                    let unit = if hours == 1 {
                        language.text("Stunde", "hour")
                    } else {
                        language.text("Stunden", "hours")
                    };
                    format!("-{} {} ({:02}:{:02})", hours, unit, h, m)
                }
            } else if let Some((h, m)) = fh.exhaustion_time {
                if let Some(delta) = fh.delta_minutes {
                    if fh.runway_minutes.unwrap_or(0) >= 1440 || delta >= 1440 {
                        match language {
                            Language::German => "+>24h Reserve".to_string(),
                            Language::English => "+>24h reserve".to_string(),
                        }
                    } else if delta > 300 {
                        match language {
                            Language::German => "+>5h Reserve".to_string(),
                            Language::English => "+>5h reserve".to_string(),
                        }
                    } else if delta >= 60 {
                        let hours = delta / 60;
                        let mins = delta % 60;
                        if mins == 0 {
                            match language {
                                Language::German => format!("+{}h Reserve ({:02}:{:02})", hours, h, m),
                                Language::English => format!("+{}h reserve ({:02}:{:02})", hours, h, m),
                            }
                        } else {
                            match language {
                                Language::German => format!("+{}h {}m Reserve ({:02}:{:02})", hours, mins, h, m),
                                Language::English => format!("+{}h {}m reserve ({:02}:{:02})", hours, mins, h, m),
                            }
                        }
                    } else if delta > 0 {
                        match language {
                            Language::German => format!("+{} Min. Reserve ({:02}:{:02})", delta, h, m),
                            Language::English => format!("+{} min reserve ({:02}:{:02})", delta, h, m),
                        }
                    } else {
                        format!("±0 Min. ({:02}:{:02})", h, m)
                    }
                } else {
                    match language {
                        Language::German => "+>24h Reserve".to_string(),
                        Language::English => "+>24h reserve".to_string(),
                    }
                }
            } else {
                match language {
                    Language::German => "+>24h Reserve".to_string(),
                    Language::English => "+>24h reserve".to_string(),
                }
            }
        } else if let Some(ratio) = self.pace_ratio {
            format!("Pace {:.1}x", ratio)
        } else {
            language.text("Bereit", "Ready").to_string()
        }
    }

    pub fn localized_lower_line(&self, language: Language) -> String {
        if self.health == PacingHealth::Throttled {
            let reset = self.reset_str.as_deref().unwrap_or(language.text("bald", "soon"));
            return format!("Reset {reset}");
        }

        if let Some(delta) = self.delta_days {
            if delta < 0 {
                let days = (-delta).max(1);
                let unit = if days == 1 {
                    language.text("Tag", "day")
                } else {
                    language.text("Tage", "days")
                };
                if let Some((m, d)) = self.week_exhaustion_date {
                    let date_str = match language {
                        Language::German => format!("{:02}.{:02}.", d, m),
                        Language::English => format!("{:02}.{:02}", d, m),
                    };
                    format!("-{} {} ({})", days, unit, date_str)
                } else {
                    format!("-{} {}", days, unit)
                }
            } else if delta == 0 {
                if let Some(rw) = self.runway_days {
                    let diff = rw - self.remaining_days;
                    let hours = (diff * 24.0).round() as i32;
                    if hours > 0 {
                        return format!("+{}h {}", hours, language.text("Reserve", "reserve"));
                    } else if hours < 0 {
                        return format!("-{}h {}", -hours, language.text("Defizit", "deficit"));
                    }
                }
                let reset = self.reset_str.as_deref().unwrap_or("—");
                match language {
                    Language::German => format!("±0 Tage ({reset})"),
                    Language::English => format!("±0 days ({reset})"),
                }
            } else if delta > 14 {
                match language {
                    Language::German => "+>30 Tage Reserve".to_string(),
                    Language::English => "+>30 days reserve".to_string(),
                }
            } else if delta > 7 {
                match language {
                    Language::German => "+>7 Tage Reserve".to_string(),
                    Language::English => "+>7 days reserve".to_string(),
                }
            } else {
                let days = delta;
                let unit = if days == 1 {
                    language.text("Tag", "day")
                } else {
                    language.text("Tage", "days")
                };
                if let Some((m, d)) = self.week_exhaustion_date {
                    let date_str = match language {
                        Language::German => format!("{:02}.{:02}.", d, m),
                        Language::English => format!("{:02}.{:02}", d, m),
                    };
                    format!("+{} {} ({})", days, unit, date_str)
                } else {
                    format!("+{} {} {}", days, unit, language.text("Reserve", "reserve"))
                }
            }
        } else if let Some((m, d)) = self.week_exhaustion_date {
            let date_str = match language {
                Language::German => format!("{:02}.{:02}.", d, m),
                Language::English => format!("{:02}.{:02}", d, m),
            };
            match language {
                Language::German => format!("+Reserve ({})", date_str),
                Language::English => format!("+Reserve ({})", date_str),
            }
        } else if self.delta_days.is_none() && self.reset_str.is_none() && self.runway_days.is_none() {
            language.text("Telemetrie ausstehend", "Telemetry pending").to_string()
        } else {
            match language {
                Language::German => "+>30 Tage Reserve".to_string(),
                Language::English => "+>30 days reserve".to_string(),
            }
        }
    }

    #[allow(dead_code)]
    pub fn localized_badge_text(&self, language: Language) -> String {
        self.localized_upper_line(language)
    }

    #[allow(dead_code)]
    pub fn localized_summary_text(&self, language: Language) -> String {
        self.localized_lower_line(language)
    }

    pub fn five_hour_runway_text(&self, language: Language) -> String {
        if let Some(fh) = &self.five_hour_forecast {
            if fh.is_exhausted_before_reset {
                let mins = fh.runway_minutes.unwrap_or(0);
                let time_suffix = if let Some((h, m)) = fh.exhaustion_time {
                    format!(" ({h:02}:{m:02})")
                } else {
                    String::new()
                };
                if mins >= 60 {
                    let h = mins / 60;
                    let m = mins % 60;
                    if m == 0 {
                        format!("{}: ~{h}h{time_suffix}", language.text("Leer in", "Empty in"))
                    } else {
                        format!("{}: ~{h}h {m}m{time_suffix}", language.text("Leer in", "Empty in"))
                    }
                } else {
                    format!("{}: ~{mins}m{time_suffix}", language.text("Leer in", "Empty in"))
                }
            } else if let Some(mins) = fh.runway_minutes {
                let time_suffix = if let Some((h, m)) = fh.exhaustion_time {
                    format!(" ({h:02}:{m:02})")
                } else {
                    String::new()
                };
                if mins >= 1440 {
                    language.text("Reicht >24h", "Lasts >24h").to_string()
                } else if mins >= 60 {
                    let h = (mins as f32 / 60.0).round() as u32;
                    format!("{} ~{h}h{time_suffix}", language.text("Reicht noch", "Lasts"))
                } else {
                    format!("{} ~{mins}m{time_suffix}", language.text("Reicht noch", "Lasts"))
                }
            } else {
                language.text("Reicht >24h", "Lasts >24h").to_string()
            }
        } else {
            language.text("Kein Limit", "No limit").to_string()
        }
    }

    pub fn five_hour_pace_text(&self, language: Language) -> String {
        if let Some(fh) = &self.five_hour_forecast {
            let ratio = fh.pace_ratio;
            let qualifier = if fh.is_exhausted_before_reset {
                let def = fh.delta_minutes.map(|d| (-d).max(1)).unwrap_or(30);
                if def < 60 {
                    format!("-{}m {}", def, language.text("Defizit", "deficit"))
                } else {
                    let h = def / 60;
                    let m = def % 60;
                    if m == 0 {
                        format!("-{}h {}", h, language.text("Defizit", "deficit"))
                    } else {
                        format!("-{}h {}m {}", h, m, language.text("Defizit", "deficit"))
                    }
                }
            } else if let Some(delta) = fh.delta_minutes {
                if fh.runway_minutes.unwrap_or(0) >= 1440 || delta >= 1440 {
                    language.text("+>24h Reserve", "+>24h reserve").to_string()
                } else if delta > 300 {
                    language.text("+>5h Reserve", "+>5h reserve").to_string()
                } else if delta >= 60 {
                    let h = delta / 60;
                    let m = delta % 60;
                    if m == 0 {
                        format!("+{}h {}", h, language.text("Reserve", "reserve"))
                    } else {
                        format!("+{}h {}m {}", h, m, language.text("Reserve", "reserve"))
                    }
                } else if delta > 0 {
                    format!("+{}m {}", delta, language.text("Reserve", "reserve"))
                } else {
                    language.text("±0m Reserve", "±0m reserve").to_string()
                }
            } else {
                language.text("Reserve", "reserve").to_string()
            };
            format!("Pace: {:.2}x · {}", ratio, qualifier)
        } else {
            String::new()
        }
    }

    pub fn week_runway_text(&self, language: Language) -> String {
        match self.week_state {
            WeekForecastState::ConfirmedExhausted => {
                return language.text("Erschöpft", "Depleted").to_string();
            }
            WeekForecastState::Pending(reason) => {
                return match reason {
                    WeekPendingReason::EarlyWindowAge | WeekPendingReason::LowEarlyUsage => {
                        language
                            .text("Schätzung ausstehend", "Estimate pending")
                            .to_string()
                    }
                    WeekPendingReason::ZeroUsage => {
                        language.text("Reichweite offen", "Runway open").to_string()
                    }
                    WeekPendingReason::InvalidOrMissing => language
                        .text("Telemetrie ausstehend", "Telemetry pending")
                        .to_string(),
                };
            }
            WeekForecastState::Active
            | WeekForecastState::ProvenDeficit
            | WeekForecastState::EstimateNeutral => {}
        }
        if let Some(days) = self.runway_days {
            let date_suffix = if let Some((m, d)) = self.week_exhaustion_date {
                format!(" ({d:02}.{m:02})")
            } else {
                String::new()
            };
            if self.week_is_estimate {
                if days > 30.0 {
                    language.text("Reicht ~>30 Tage", "Lasts ~>30 days").to_string()
                } else if days >= 1.0 {
                    match language {
                        Language::German => format!("Reicht ~{:.1} Tage{date_suffix}", days),
                        Language::English => format!("Lasts ~{:.1} days{date_suffix}", days),
                    }
                } else {
                    let hours = (days * 24.0).round() as u32;
                    match language {
                        Language::German => format!("Reicht ~{}h{date_suffix}", hours),
                        Language::English => format!("Lasts ~{}h{date_suffix}", hours),
                    }
                }
            } else if days >= 30.0 {
                language.text("Reicht >30 Tage", "Lasts >30 days").to_string()
            } else if days >= 1.0 {
                match language {
                    Language::German => format!("Reicht noch {:.1} Tage{date_suffix}", days),
                    Language::English => format!("Lasts {:.1} days{date_suffix}", days),
                }
            } else {
                let hours = (days * 24.0).round() as u32;
                match language {
                    Language::German => format!("Reicht noch ~{}h{date_suffix}", hours),
                    Language::English => format!("Lasts ~{}h{date_suffix}", hours),
                }
            }
        } else {
            String::new()
        }
    }

    pub fn week_pace_text(&self, language: Language) -> String {
        match self.week_state {
            WeekForecastState::ConfirmedExhausted => return String::new(),
            WeekForecastState::Pending(reason) => {
                return match reason {
                    WeekPendingReason::EarlyWindowAge => language
                        .text("Datenbasis zu kurz", "Window too young")
                        .to_string(),
                    WeekPendingReason::LowEarlyUsage => language
                        .text("Verbrauch zu gering", "Usage too low")
                        .to_string(),
                    WeekPendingReason::ZeroUsage => {
                        language.text("Kein Verbrauch", "No usage").to_string()
                    }
                    WeekPendingReason::InvalidOrMissing => String::new(),
                };
            }
            WeekForecastState::Active
            | WeekForecastState::ProvenDeficit
            | WeekForecastState::EstimateNeutral => {}
        }
        if let Some(ratio) = self.pace_ratio {
            let qualifier = if let Some(runway) = self.runway_days {
                let diff = self.projected_week_reserve.unwrap_or(runway - self.remaining_days);
                let is_proven_deficit = self.week_state == WeekForecastState::ProvenDeficit
                    || (self.week_state == WeekForecastState::Active && diff < -1e-6);
                if diff < -0.04 || is_proven_deficit {
                    if diff <= -1.5 {
                        let d = (-diff).round() as i32;
                        format!("-{}d {}", d, language.text("Defizit", "deficit"))
                    } else if diff <= -0.85 {
                        format!("-1d {}", language.text("Defizit", "deficit"))
                    } else if diff > -(1.0 / 24.0) {
                        let mins = ((-diff) * 1440.0).round().max(1.0) as i32;
                        format!("-{}m {}", mins, language.text("Defizit", "deficit"))
                    } else {
                        let h = ((-diff) * 24.0).round() as i32;
                        if h >= 20 {
                            format!("-1d {}", language.text("Defizit", "deficit"))
                        } else {
                            format!("-{}h {}", h.max(1), language.text("Defizit", "deficit"))
                        }
                    }
                } else if diff > 0.04 {
                    if diff > 30.0 {
                        language.text("+>30 Tage Reserve", "+>30 days reserve").to_string()
                    } else if diff >= 1.5 {
                        let d = (diff.round() as i32).min(30);
                        format!("+{}d {}", d, language.text("Reserve", "reserve"))
                    } else if diff >= 0.85 {
                        format!("+1d {}", language.text("Reserve", "reserve"))
                    } else {
                        let h = (diff * 24.0).round() as i32;
                        if h >= 20 {
                            format!("+1d {}", language.text("Reserve", "reserve"))
                        } else {
                            format!("+{}h {}", h.max(1), language.text("Reserve", "reserve"))
                        }
                    }
                } else {
                    language.text("±0h Reserve", "±0h reserve").to_string()
                }
            } else if let Some(delta) = self.delta_days {
                if delta < 0 {
                    let d = (-delta).max(1);
                    format!("-{}d {}", d, language.text("Defizit", "deficit"))
                } else if delta > 30 {
                    language.text("+>30 Tage Reserve", "+>30 days reserve").to_string()
                } else if delta > 0 {
                    format!("+{}d {}", delta, language.text("Reserve", "reserve"))
                } else {
                    language.text("±0d Reserve", "±0d reserve").to_string()
                }
            } else if ratio < 1.0 {
                language.text("Reserve", "reserve").to_string()
            } else if ratio <= 1.25 {
                language.text("Ausgeglichen", "on track").to_string()
            } else {
                language.text("Erhöht", "elevated").to_string()
            };
            if self.week_is_estimate {
                format!("Pace: ~{:.2}x · ~{}", ratio, qualifier)
            } else {
                format!("Pace: {:.2}x · {}", ratio, qualifier)
            }
        } else {
            String::new()
        }
    }
}

pub fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub fn ymd_to_days(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let m = if month <= 2 { month + 9 } else { month - 3 } as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Used only by the non-Windows fallback path of the date helpers.
#[cfg(not(windows))]
pub fn current_ymd() -> (i32, u32, u32) {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86400) as i64;
    days_to_ymd(days)
}

pub fn current_hm() -> (u32, u32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe { GetLocalTime(&mut st) };
        (st.wHour as u32, st.wMinute as u32)
    }
    #[cfg(not(windows))]
    {
        let now = std::time::SystemTime::now();
        let secs = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let hour = ((secs % 86400) / 3600) as u32;
        let minute = ((secs % 3600) / 60) as u32;
        (hour, minute)
    }
}

fn current_forecast_today() -> (i32, u32, u32) {
    #[cfg(windows)]
    {
        let (year, month, day, _, _, _) = current_local_datetime();
        (year as i32, month, day)
    }
    #[cfg(not(windows))]
    {
        current_ymd()
    }
}

#[allow(dead_code)]
pub fn current_local_datetime() -> (u32, u32, u32, u32, u32, u32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe { GetLocalTime(&mut st) };
        (
            st.wYear as u32,
            st.wMonth as u32,
            st.wDay as u32,
            st.wDayOfWeek as u32,
            st.wHour as u32,
            st.wMinute as u32,
        )
    }
    #[cfg(not(windows))]
    {
        let (y, m, d) = current_ymd();
        let (h, min) = current_hm();
        // Fallback weekday calculation
        (y as u32, m, d, 1, h, min)
    }
}

#[cfg(windows)]
pub fn local_timezone_offset_minutes() -> i32 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let mut local = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe {
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
    }
    let local_mins = local.wHour as i32 * 60 + local.wMinute as i32;
    let utc_mins = utc.wHour as i32 * 60 + utc.wMinute as i32;
    let mut diff = local_mins - utc_mins;
    if local.wDay != utc.wDay {
        if local.wDay > utc.wDay || (local.wDay == 1 && utc.wDay > 25) {
            diff += 24 * 60;
        } else {
            diff -= 24 * 60;
        }
    }
    diff
}

#[cfg(not(windows))]
pub fn local_timezone_offset_minutes() -> i32 {
    120
}

pub fn iso_utc_to_local_hm(iso_str: &str) -> Option<(u32, u32)> {
    let raw = iso_str.trim();
    let time_part = if let Some(t_idx) = raw.find('T') {
        &raw[t_idx + 1..]
    } else {
        raw
    };
    let is_utc = time_part.ends_with('Z') || raw.ends_with('Z');
    let clean = time_part.trim_end_matches('Z');
    let parts: Vec<&str> = clean.split(':').collect();
    if parts.len() < 2 {
        return None;
    }
    let h: u32 = parts[0].trim().parse().ok()?;
    let m: u32 = parts[1].trim().parse().ok()?;

    if is_utc {
        let offset = local_timezone_offset_minutes();
        let total_mins = (h as i32 * 60 + m as i32 + offset).rem_euclid(24 * 60);
        let local_h = (total_mins / 60) as u32;
        let local_m = (total_mins % 60) as u32;
        Some((local_h, local_m))
    } else {
        Some((h, m))
    }
}

pub fn parse_hm(time_str: Option<&str>) -> Option<(u32, u32)> {
    let raw = time_str?.trim();
    if raw.contains('T') || raw.ends_with('Z') {
        iso_utc_to_local_hm(raw)
    } else {
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() >= 2 {
            let h: u32 = parts[0].trim().parse().ok()?;
            let m: u32 = parts[1].trim().parse().ok()?;
            Some((h, m))
        } else {
            None
        }
    }
}

pub fn calculate_five_hour_forecast(
    five_hour_percent: Option<u8>,
    five_hour_reset: Option<&str>,
    now_hm: (u32, u32),
) -> Option<FiveHourForecast> {
    let rem_pct = five_hour_percent?;
    let (reset_h, reset_m) = parse_hm(five_hour_reset)?;

    let curr_mins = now_hm.0 * 60 + now_hm.1;
    let reset_mins = reset_h * 60 + reset_m;

    let mut diff_mins = reset_mins as i32 - curr_mins as i32;
    if diff_mins <= 0 {
        diff_mins += 24 * 60;
    }

    // A rolling 5-hour window has a maximum cycle duration of 300 minutes.
    // If diff_mins > 300, the recorded reset timestamp is from an earlier cycle that has already completed and reset!
    if diff_mins > 300 {
        return Some(FiveHourForecast {
            burn_rate: 0.0,
            pace_ratio: 0.0,
            runway_minutes: Some(1440),
            delta_minutes: Some(1440),
            exhaustion_time: None,
            reset_time: (reset_h, reset_m),
            is_exhausted_before_reset: false,
        });
    }

    let remaining_mins = (diff_mins as u32).min(300).max(1);
    let elapsed_mins = (300u32.saturating_sub(remaining_mins)).max(10);

    let consumed_pct = (100u8.saturating_sub(rem_pct)) as f32;
    let burn_rate = consumed_pct / (elapsed_mins as f32);
    let allowed_rate = 100.0 / 300.0;
    let pace_ratio = burn_rate / allowed_rate;

    if consumed_pct <= 0.0 || burn_rate <= 0.0001 {
        return Some(FiveHourForecast {
            burn_rate: 0.0,
            pace_ratio: 0.0,
            runway_minutes: Some(1440),
            delta_minutes: Some(1440),
            exhaustion_time: None,
            reset_time: (reset_h, reset_m),
            is_exhausted_before_reset: false,
        });
    }

    let runway_mins = ((rem_pct as f32) / burn_rate).round() as u32;
    let is_exhausted_before_reset = runway_mins < remaining_mins;
    let delta_minutes = Some(runway_mins as i32 - remaining_mins as i32);

    let empty_mins = curr_mins + runway_mins;
    let ex_h = (empty_mins / 60) % 24;
    let ex_m = empty_mins % 60;

    Some(FiveHourForecast {
        burn_rate,
        pace_ratio,
        runway_minutes: Some(runway_mins),
        delta_minutes,
        exhaustion_time: Some((ex_h, ex_m)),
        reset_time: (reset_h, reset_m),
        is_exhausted_before_reset,
    })
}

pub fn parse_remaining_days(reset_str: Option<&str>, today: (i32, u32, u32)) -> Option<f32> {
    let raw = reset_str?.trim();
    let today_days = ymd_to_days(today.0, today.1, today.2);

    // Format 1: "25.09." or "25.09" or "25.09. 12:53"
    if raw.contains('.') {
        let date_part = raw.split_whitespace().next().unwrap_or(raw);
        let parts: Vec<&str> = date_part.split('.').filter(|s| !s.trim().is_empty()).collect();
        if parts.len() >= 2 {
            let day: u32 = parts[0].trim().parse().ok()?;
            let month: u32 = parts[1].trim().parse().ok()?;
            let mut year = today.0;
            if month < today.1 {
                year += 1;
            }
            let target_days = ymd_to_days(year, month, day);
            let diff = target_days - today_days;
            return Some((diff.max(0) as f32).max(0.1));
        }
    }

    // Format 2: ISO string like "2026-09-25T10:53:00Z" or "2026-09-25"
    if raw.contains('-') {
        let date_part = raw.split('T').next()?;
        let parts: Vec<&str> = date_part.split('-').collect();
        if parts.len() >= 3 {
            let year: i32 = parts[0].trim().parse().ok()?;
            let month: u32 = parts[1].trim().parse().ok()?;
            let day: u32 = parts[2].trim().parse().ok()?;
            let target_days = ymd_to_days(year, month, day);
            let diff = target_days - today_days;
            return Some((diff.max(0) as f32).max(0.1));
        }
    }

    None
}

fn remaining_days_at(
    reset_str: Option<&str>,
    today: (i32, u32, u32),
    now: (u32, u32),
) -> Option<f32> {
    let (target, time) = parse_target_datetime(reset_str?, today)?;
    if let Some((hour, minute)) = time {
        let days = ymd_to_days(target.0, target.1, target.2)
            - ymd_to_days(today.0, today.1, today.2);
        let minutes = days * 1440 + hour as i64 * 60 + minute as i64
            - now.0 as i64 * 60 - now.1 as i64;
        Some((minutes as f32 / 1440.0).max(0.0))
    } else {
        // Date-only providers do not supply enough precision for an hourly pace.
        parse_remaining_days(reset_str, today)
    }
}

pub fn parse_target_datetime(raw: &str, today: (i32, u32, u32)) -> Option<((i32, u32, u32), Option<(u32, u32)>)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Format A: ISO string "2026-09-25T10:53:00Z" or "2026-09-25"
    if raw.contains('-') && (raw.contains('T') || raw.split('-').count() >= 3) {
        let date_part = raw.split('T').next()?;
        let parts: Vec<&str> = date_part.split('-').collect();
        if parts.len() >= 3 {
            let y: i32 = parts[0].trim().parse().ok()?;
            let m: u32 = parts[1].trim().parse().ok()?;
            let d: u32 = parts[2].trim().parse().ok()?;
            let time_opt = iso_utc_to_local_hm(raw);
            return Some(((y, m, d), time_opt));
        }
    }

    // Format B: "23.09. (19:00)", "23.09. 19:00", "25.09.", "25.09", "am 23.09. (19:00)"
    if raw.contains('.') {
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let date_token = tokens.iter().find(|t| t.contains('.'))?;
        let date_parts: Vec<&str> = date_token.split('.').filter(|s| !s.trim().is_empty()).collect();
        if date_parts.len() >= 2 {
            let d: u32 = date_parts[0].trim().parse().ok()?;
            let m: u32 = date_parts[1].trim().parse().ok()?;
            let mut y = today.0;
            if m < today.1 {
                y += 1;
            }

            let mut time_opt = None;
            for token in &tokens {
                if token.contains(':') {
                    let cleaned: String = token.chars().filter(|c| c.is_ascii_digit() || *c == ':').collect();
                    let hm_parts: Vec<&str> = cleaned.split(':').collect();
                    if hm_parts.len() >= 2 {
                        if let (Ok(h), Ok(min)) = (hm_parts[0].parse::<u32>(), hm_parts[1].parse::<u32>()) {
                            time_opt = Some((h, min));
                            break;
                        }
                    }
                }
            }

            return Some(((y, m, d), time_opt));
        }
    }

    None
}

pub fn format_reset_time_phrase(
    reset_raw: Option<&str>,
    is_five_hour: bool,
    language: Language,
    now_dt: (i32, u32, u32, u32, u32), // (year, month, day, hour, min)
) -> String {
    let Some(raw) = reset_raw.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return match language {
            Language::German => "nicht terminiert".to_string(),
            Language::English => "no schedule".to_string(),
        };
    };

    if is_five_hour && raw.contains(':') {
        if let Some((rh, rm)) = parse_hm(Some(raw)) {
            let curr_mins = now_dt.3 * 60 + now_dt.4;
            let reset_mins = rh * 60 + rm;
            let mut diff_mins = reset_mins as i32 - curr_mins as i32;
            if diff_mins < 0 {
                diff_mins += 24 * 60;
            }
            let hours = diff_mins / 60;
            let mins = diff_mins % 60;
            if diff_mins > 300 {
                return match language {
                    Language::German => format!("um {rh:02}:{rm:02}"),
                    Language::English => format!("at {rh:02}:{rm:02}"),
                };
            } else if hours > 0 {
                return format!("in {hours}h {mins}m ({rh:02}:{rm:02})");
            } else {
                return format!("in {mins}m ({rh:02}:{rm:02})");
            }
        }
        return raw.to_string();
    }

    if raw.starts_with("in ") {
        return raw.to_string();
    }

    let Some(((target_y, target_m, target_d), time_opt)) =
        parse_target_datetime(raw, (now_dt.0, now_dt.1, now_dt.2))
    else {
        return fallback_raw_format(raw, language);
    };

    let now_days = ymd_to_days(now_dt.0, now_dt.1, now_dt.2);
    let target_days = ymd_to_days(target_y, target_m, target_d);

    let (diff_mins, has_exact_time) = match time_opt {
        Some((th, tm)) => {
            let now_total = now_days * 1440 + now_dt.3 as i64 * 60 + now_dt.4 as i64;
            let target_total = target_days * 1440 + th as i64 * 60 + tm as i64;
            (target_total - now_total, true)
        }
        None => {
            let diff_days = target_days - now_days;
            (diff_days * 1440, false)
        }
    };

    if diff_mins <= 0 {
        return match time_opt {
            Some((th, tm)) => {
                let time_str = format!("{th:02}:{tm:02}");
                format!("{} ({time_str})", language.text("in Kürze", "soon"))
            }
            None => language.text("in Kürze", "soon").to_string(),
        };
    }

    if diff_mins < 60 && has_exact_time {
        let (th, tm) = time_opt.unwrap();
        let time_str = format!("{th:02}:{tm:02}");
        return if target_days == now_days {
            match language {
                Language::German => format!("in {diff_mins}m (heute {time_str})"),
                Language::English => format!("in {diff_mins}m (today {time_str})"),
            }
        } else {
            format!("in {diff_mins}m ({target_d:02}.{target_m:02}. {time_str})")
        };
    }

    let total_hours = ((diff_mins as f64) / 60.0).round() as u64;
    let total_hours = total_hours.max(1);

    if has_exact_time && total_hours > 24 && total_hours <= 240 {
        let days = total_hours / 24;
        let hours = total_hours % 24;
        let duration = if hours == 0 {
            format!("~{days}d")
        } else {
            format!("~{days}d {hours}h")
        };
        let (th, tm) = time_opt.expect("exact reset time was checked above");
        return format!(
            "in {duration} ({target_d:02}.{target_m:02}. {th:02}:{tm:02})"
        );
    }

    if target_days == now_days {
        return match time_opt {
            Some((th, tm)) => {
                let time_str = format!("{th:02}:{tm:02}");
                match language {
                    Language::German => format!("in ~{total_hours}h (heute {time_str})"),
                    Language::English => format!("in ~{total_hours}h (today {time_str})"),
                }
            }
            None => match language {
                Language::German => format!("in ~{total_hours}h (heute)"),
                Language::English => format!("in ~{total_hours}h (today)"),
            },
        };
    }

    if total_hours <= 240 {
        return match time_opt {
            Some((th, tm)) => {
                let time_str = format!("{th:02}:{tm:02}");
                format!("in ~{total_hours}h ({target_d:02}.{target_m:02}. {time_str})")
            }
            None => format!("in ~{total_hours}h ({target_d:02}.{target_m:02}.)"),
        };
    }

    let days = ((diff_mins as f64) / 1440.0).round() as u64;
    match time_opt {
        Some((th, tm)) => {
            let time_str = format!("{th:02}:{tm:02}");
            format!("in ~{days}d ({target_d:02}.{target_m:02}. {time_str})")
        }
        None => format!("in ~{days}d ({target_d:02}.{target_m:02}.)"),
    }
}

fn fallback_raw_format(raw: &str, language: Language) -> String {
    if raw.starts_with("in ") {
        raw.to_string()
    } else if raw.contains('.') {
        let formatted = if raw.contains(':') && !raw.contains('(') {
            let parts: Vec<&str> = raw.split_whitespace().collect();
            if parts.len() >= 2 && parts[1].contains(':') {
                format!("{} ({})", parts[0], parts[1])
            } else {
                raw.to_string()
            }
        } else {
            raw.to_string()
        };
        match language {
            Language::German => format!("am {formatted}"),
            Language::English => format!("on {formatted}"),
        }
    } else {
        format!("in {raw}")
    }
}

// Deterministic forecast entry point for the test matrix; production paths
// go through the live calculate_* helpers.
#[cfg(test)]
pub fn pacing_forecast_for(
    is_throttled: bool,
    week_percent: Option<u8>,
    week_reset: Option<&str>,
    five_hour_percent: Option<u8>,
    five_hour_reset: Option<&str>,
    custom_today: Option<(i32, u32, u32)>,
    custom_time: Option<(u32, u32)>,
) -> PacingForecast {
    pacing_forecast_with_cycle_days(
        is_throttled,
        week_percent,
        week_reset,
        five_hour_percent,
        five_hour_reset,
        None,
        None,
        custom_today,
        custom_time,
    )
}

/// Traffic-light decision for one limit line, extracted from the satellite
/// renderer so it is unit-testable with the exact inputs the app sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunwayLight {
    pub color: RunwayColor,
    pub red: bool,
    pub yellow: bool,
}

/// The four traffic-light states a limit line can render, gray for an
/// unusable (throttled) provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunwayColor {
    Gray,
    Red,
    Yellow,
    Green,
}

impl RunwayColor {
    /// RGB triple matching the satellite's pastel traffic-light palette.
    /// Only the Windows satellite renderer consumes this method, so the
    /// band keeps non-Windows builds free of a false dead-code warning.
    #[cfg(windows)]
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            RunwayColor::Gray => (148, 163, 184),
            RunwayColor::Red => (252, 165, 165),
            RunwayColor::Yellow => (253, 224, 71),
            RunwayColor::Green => (120, 184, 134),
        }
    }
}

pub fn runway_light(
    is_throttled: bool,
    pct: u8,
    is_five_hour: bool,
    forecast: &PacingForecast,
) -> RunwayLight {
    let light = |color: RunwayColor, red: bool, yellow: bool| RunwayLight { color, red, yellow };
    if is_throttled {
        return light(RunwayColor::Gray, false, false);
    }
    if !is_five_hour
        && matches!(
            forecast.week_state,
            WeekForecastState::EstimateNeutral | WeekForecastState::Pending(_)
        )
    {
        return light(RunwayColor::Gray, pct < 10, pct >= 10 && pct < 20);
    }
    if pct == 0 {
        return light(RunwayColor::Red, true, false);
    }
    if is_five_hour {
        if let Some(fh) = &forecast.five_hour_forecast {
            if fh.is_exhausted_before_reset {
                let def = fh.delta_minutes.unwrap_or(0);
                return if def < -45 {
                    light(RunwayColor::Red, true, false)
                } else {
                    light(RunwayColor::Yellow, false, true)
                };
            }
            return if pct < 20 || (pct < 30 && fh.pace_ratio > 1.15) {
                light(RunwayColor::Yellow, false, true)
            } else {
                light(RunwayColor::Green, false, false)
            };
        }
        return if pct < 20 {
            light(RunwayColor::Yellow, false, true)
        } else {
            light(RunwayColor::Green, false, false)
        };
    }
    if forecast.week_state == WeekForecastState::ConfirmedExhausted {
        return light(RunwayColor::Red, true, false);
    }
    let diff_days = forecast.runway_days.map(|rw| rw - forecast.remaining_days);
    let shielded = forecast.week_deficit_shielded(pct);
    if pct < 10 {
        return light(RunwayColor::Red, true, false);
    }
    if let Some(diff) = diff_days {
        if diff < -0.85 && !shielded {
            return light(RunwayColor::Red, true, false);
        }
        if diff < -0.04 || forecast.week_state == WeekForecastState::ProvenDeficit {
            return light(RunwayColor::Yellow, false, true);
        }
        if pct < 15 || (pct < 25 && forecast.pace_ratio.map_or(false, |r| r > 1.15)) {
            return light(RunwayColor::Yellow, false, true);
        }
        return light(RunwayColor::Green, false, false);
    }
    if pct < 20 {
        return light(RunwayColor::Yellow, false, true);
    }
    light(RunwayColor::Green, false, false)
}

/// Pace-line color: it inherits the runway alert, and otherwise follows the
/// pace ratio itself (green under 1.0, yellow to 1.25, red above).
pub fn pace_light(
    is_throttled: bool,
    pct: u8,
    runway: &RunwayLight,
    pace_ratio: Option<f32>,
) -> RunwayColor {
    if is_throttled {
        return RunwayColor::Gray;
    }
    if pct == 0 || runway.red {
        return RunwayColor::Red;
    }
    if runway.yellow {
        return RunwayColor::Yellow;
    }
    match pace_ratio {
        Some(ratio) if ratio < 1.0 => RunwayColor::Green,
        Some(ratio) if ratio <= 1.25 => RunwayColor::Yellow,
        Some(_) => RunwayColor::Red,
        None => RunwayColor::Gray,
    }
}

/// Shared week-line pace color governed by `forecast.week_state`: neutral
/// estimates and pending states never tint green/yellow/red from pacing, while
/// independent quota-level alerts (`runway.red` / `runway.yellow` / `pct == 0`)
/// still surface.
pub fn week_pace_light(
    is_throttled: bool,
    pct: u8,
    runway: &RunwayLight,
    forecast: &PacingForecast,
) -> RunwayColor {
    if is_throttled
        || matches!(
            forecast.week_state,
            WeekForecastState::EstimateNeutral | WeekForecastState::Pending(_)
        )
    {
        return RunwayColor::Gray;
    }
    pace_light(is_throttled, pct, runway, forecast.pace_ratio)
}

pub fn pacing_forecast_with_cycle_days(
    is_throttled: bool,
    week_percent: Option<u8>,
    week_reset: Option<&str>,
    five_hour_percent: Option<u8>,
    five_hour_reset: Option<&str>,
    cycle_days: Option<f32>,
    consumed_override: Option<f32>,
    custom_today: Option<(i32, u32, u32)>,
    custom_time: Option<(u32, u32)>,
) -> PacingForecast {
    let today = custom_today.unwrap_or_else(current_forecast_today);
    let now_hm = custom_time.unwrap_or_else(current_hm);

    let five_hour_forecast = calculate_five_hour_forecast(five_hour_percent, five_hour_reset, now_hm);
    // Silent forecast paths must keep a session-window exhaustion visible.
    let silent_health = if five_hour_forecast
        .as_ref()
        .map(|fh| fh.is_exhausted_before_reset)
        .unwrap_or(false)
    {
        PacingHealth::Tight
    } else {
        PacingHealth::Surplus
    };

    let rem_pct = week_percent.or(five_hour_percent).unwrap_or(100);
    let saved_reset = week_reset.map(|s| s.to_string());

    if week_percent.is_none() && five_hour_percent.is_none() {
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: None,
            delta_days: None,
            remaining_days: 0.0,
            health: PacingHealth::Surplus,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: None,
            week_exhaustion_date: None,
            reset_str: None,
            today,
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    let week_is_exhausted = match week_percent {
        Some(w) => w == 0,
        None => is_throttled || rem_pct == 0,
    };

    if week_is_exhausted {
        let rem_days = remaining_days_at(week_reset, today, now_hm).unwrap_or(5.0);
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: Some(0.0),
            delta_days: Some(-(rem_days.round() as i32)),
            remaining_days: rem_days,
            health: PacingHealth::Throttled,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast,
            week_exhaustion_date: None,
            reset_str: saved_reset,
            today,
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    // A remaining percentage alone does not reveal how much of the quota
    // period has elapsed. Without a usable reset date, a burn-rate forecast
    // would have to invent the elapsed time and can produce wildly misleading
    // runway and pace values.
    let remaining_days = match remaining_days_at(week_reset, today, now_hm) {
        Some(days) => days.max(1.0 / 1440.0),
        None if week_percent.is_some() => {
            let mut fc = PacingForecast {
                pace_ratio: None,
                runway_days: None,
                delta_days: None,
                remaining_days: 0.0,
                health: silent_health,
                badge_text: String::new(),
                summary_text: String::new(),
                five_hour_forecast,
                week_exhaustion_date: None,
                reset_str: saved_reset,
                today,
                week_state: WeekForecastState::Active,
                week_is_estimate: false,
                projected_week_reserve: None,
            };
            fc.badge_text = fc.localized_upper_line(Language::German);
            fc.summary_text = fc.localized_lower_line(Language::German);
            return fc;
        }
        None => 7.0,
    };
    let total_days = cycle_days.unwrap_or(if remaining_days > 7.5 { 30.0 } else { 7.0 });
    let has_reset_time = week_reset
        .and_then(|raw| parse_target_datetime(raw, today))
        .is_some_and(|(_, time)| time.is_some());
    // Exact reset times reveal the actual cycle age. A fixed 4.8-hour floor
    // invents consumption history near the start of a weekly period.
    let min_elapsed = if has_reset_time { 1.0 / 1440.0 } else { 0.2 };
    // A just-started cycle has no meaningful burn history. Without the exact
    // cycle length the guessed total collapses elapsed to the floor and turns
    // a few percent of consumption into a wildly false pace (observed 432x on
    // a three-hour-old monthly window). Show no forecast in that case.
    if cycle_days.is_none() && total_days - remaining_days <= min_elapsed {
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: None,
            delta_days: None,
            remaining_days,
            health: silent_health,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast,
            week_exhaustion_date: None,
            reset_str: saved_reset,
            today,
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }
    let elapsed_days = (total_days - remaining_days).max(min_elapsed);
    let consumed_pct = consumed_override
        .filter(|value| (0.0..=100.0).contains(value))
        .unwrap_or((100u8.saturating_sub(rem_pct)) as f32);
    // Fresh-cycle telemetry gate: while the cycle has run less than half a
    // day and consumption stays in rounding range, every runway and deficit
    // figure is a tiny denominator multiplied by noise (observed: "Reicht
    // noch 12.7 Tage · -22d Defizit" at 98 % remaining, four hours into a
    // 31-day cycle). Stay silent until the telemetry can carry a forecast.
    if elapsed_days < 0.5 && consumed_pct < 5.0 {
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: None,
            delta_days: None,
            remaining_days,
            health: silent_health,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast,
            week_exhaustion_date: None,
            reset_str: saved_reset,
            today,
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    let allowed_rate = 100.0 / total_days;
    let burn_rate = consumed_pct / elapsed_days;
    let pace_ratio = burn_rate / allowed_rate;

    let five_h_tight = five_hour_forecast
        .as_ref()
        .map(|fh| fh.is_exhausted_before_reset)
        .unwrap_or(false);

    if consumed_pct <= 2.0 || burn_rate <= 0.05 {
        let health = if five_h_tight {
            PacingHealth::Tight
        } else {
            PacingHealth::Surplus
        };

        let runway_days = if burn_rate > 0.001 {
            Some((rem_pct as f32) / burn_rate)
        } else {
            Some(30.0)
        };

        let pace = if burn_rate > 0.001 {
            pace_ratio
        } else {
            0.0
        };

        let delta_days = runway_days.map(|rw| (rw - remaining_days).round() as i32);
        let week_exhaustion_date = runway_days.and_then(|rw| {
            if rw < 30.0 {
                let today_days = ymd_to_days(today.0, today.1, today.2);
                let add_days = (rw + (now_hm.0 * 60 + now_hm.1) as f32 / 1440.0).floor() as i64;
                let (_y, ex_m, ex_d) = days_to_ymd(today_days + add_days);
                Some((ex_m, ex_d))
            } else {
                None
            }
        });

        let mut fc = PacingForecast {
            pace_ratio: Some(pace),
            runway_days,
            delta_days,
            remaining_days,
            health,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast,
            week_exhaustion_date,
            reset_str: saved_reset,
            today,
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    let runway_days = (rem_pct as f32) / burn_rate;
    let today_days = ymd_to_days(today.0, today.1, today.2);
    let add_days = (runway_days + (now_hm.0 * 60 + now_hm.1) as f32 / 1440.0).floor() as i64;
    let (_y, ex_m, ex_d) = days_to_ymd(today_days + add_days);
    let week_exhaustion_date = Some((ex_m, ex_d));
    let delta_days = Some((runway_days - remaining_days).round() as i32);

    let week_tight = if rem_pct >= 50 && pace_ratio <= 1.25 {
        // Protection shield: high remaining quota with moderate pace is never tight!
        false
    } else {
        runway_days < remaining_days * 0.80 || rem_pct < 20
    };
    let health = if week_tight || five_h_tight {
        PacingHealth::Tight
    } else if pace_ratio < 0.90 || rem_pct >= 80 {
        PacingHealth::Surplus
    } else {
        PacingHealth::OnTrack
    };

    let mut fc = PacingForecast {
        pace_ratio: Some(pace_ratio),
        runway_days: Some(runway_days),
        delta_days,
        remaining_days,
        health,
        badge_text: String::new(),
        summary_text: String::new(),
        five_hour_forecast,
        week_exhaustion_date,
        reset_str: saved_reset,
        today,
        week_state: WeekForecastState::Active,
        week_is_estimate: false,
        projected_week_reserve: None,
    };
    fc.badge_text = fc.localized_upper_line(Language::German);
    fc.summary_text = fc.localized_lower_line(Language::German);
    fc
}

fn current_utc_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub fn codex_week_pacing_forecast(
    week_percent: Option<u8>,
    week_reset: Option<&str>,
    week_reset_epoch: Option<i64>,
    week_observed_epoch: Option<i64>,
    week_consumed_pct: Option<f32>,
    week_consumed_range: Option<(f32, f32)>,
    _week_source: Option<CodexObservationSource>,
    week_confirmed_exhausted: bool,
    five_hour_percent: Option<u8>,
    five_hour_reset: Option<&str>,
    now_epoch: i64,
    today: (i32, u32, u32),
    now_hm: (u32, u32),
) -> PacingForecast {
    const CODEX_WEEK_SECS: i64 = 7 * 86_400;
    const EARLY_AGE_SECS: i64 = 3_600;
    const HALF_DAY_SECS: i64 = 43_200;

    let five_hour_forecast = calculate_five_hour_forecast(five_hour_percent, five_hour_reset, now_hm);
    let five_h_tight = five_hour_forecast
        .as_ref()
        .map(|fh| fh.is_exhausted_before_reset)
        .unwrap_or(false);
    let silent_health = if five_h_tight {
        PacingHealth::Tight
    } else {
        PacingHealth::Surplus
    };
    let saved_reset = week_reset.map(str::to_string);

    let make_forecast = |pace_ratio: Option<f32>,
                         runway_days: Option<f32>,
                         delta_days: Option<i32>,
                         remaining_days: f32,
                         health: PacingHealth,
                         week_exhaustion_date: Option<(u32, u32)>,
                         week_state: WeekForecastState,
                         week_is_estimate: bool| {
        let mut fc = PacingForecast {
            pace_ratio,
            runway_days,
            delta_days,
            remaining_days,
            health,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: five_hour_forecast.clone(),
            week_exhaustion_date,
            reset_str: saved_reset.clone(),
            today,
            week_state,
            week_is_estimate,
            projected_week_reserve: None,
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        fc
    };

    // R5/A6: Any old forecast or weekly exhaustion expires at reset, even between fetches.
    if week_reset_epoch.is_some_and(|reset_epoch| now_epoch >= reset_epoch) {
        return make_forecast(
            None,
            None,
            None,
            0.0,
            silent_health,
            None,
            WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing),
            false,
        );
    }

    // R1/A6: Confirmed weekly exhaustion only when unambiguously weekly.
    if week_confirmed_exhausted {
        let rem_days = week_reset_epoch
            .map(|r| ((r - now_epoch).max(0) as f64 / 86_400.0) as f32)
            .or_else(|| remaining_days_at(week_reset, today, now_hm))
            .unwrap_or(5.0);
        return make_forecast(
            None,
            Some(0.0),
            Some(-(rem_days.round() as i32)),
            rem_days,
            PacingHealth::Throttled,
            None,
            WeekForecastState::ConfirmedExhausted,
            false,
        );
    }

    let (Some(reset_epoch), Some(observed_epoch)) = (week_reset_epoch, week_observed_epoch) else {
        return make_forecast(
            None,
            None,
            None,
            0.0,
            silent_health,
            None,
            WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing),
            false,
        );
    };

    let window_start = reset_epoch - CODEX_WEEK_SECS;
    let elapsed_secs = observed_epoch - window_start;
    let remaining_at_obs_secs = reset_epoch - observed_epoch;
    // Reject invalid windows or observations from the future.
    if observed_epoch > now_epoch || elapsed_secs <= 0 || remaining_at_obs_secs <= 0 {
        return make_forecast(
            None,
            None,
            None,
            0.0,
            silent_health,
            None,
            WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing),
            false,
        );
    }

    let sample_age_days = (now_epoch - observed_epoch) as f64 / 86_400.0;
    let remaining_days_f64 = (reset_epoch - now_epoch) as f64 / 86_400.0;
    let remaining_at_obs_days = remaining_at_obs_secs as f64 / 86_400.0;
    let remaining_days = remaining_days_f64 as f32;
    let elapsed_days = elapsed_secs as f64 / 86_400.0;

    let Some(consumed_pct) = week_consumed_pct
        .filter(|value| (0.0..=100.0).contains(value))
        .or_else(|| week_percent.map(|rem| (100u8.saturating_sub(rem)) as f32))
    else {
        return make_forecast(
            None,
            None,
            None,
            remaining_days,
            silent_health,
            None,
            WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing),
            false,
        );
    };

    if consumed_pct <= 0.0
        && week_consumed_range.is_none_or(|(_, high)| high <= 0.0)
    {
        return make_forecast(
            None,
            None,
            None,
            remaining_days,
            silent_health,
            None,
            WeekForecastState::Pending(WeekPendingReason::ZeroUsage),
            false,
        );
    }

    let meets_threshold = if consumed_pct >= 5.0 {
        true
    } else if elapsed_secs >= HALF_DAY_SECS && consumed_pct > 0.0 {
        true
    } else {
        elapsed_secs >= EARLY_AGE_SECS && consumed_pct >= 2.0
    };

    if !meets_threshold {
        let reason = if elapsed_secs < EARLY_AGE_SECS {
            WeekPendingReason::EarlyWindowAge
        } else {
            WeekPendingReason::LowEarlyUsage
        };
        return make_forecast(
            None,
            None,
            None,
            remaining_days,
            silent_health,
            None,
            WeekForecastState::Pending(reason),
            false,
        );
    }

    let nominal_consumed = consumed_pct as f64;
    let nominal_rem_pct = (100.0 - nominal_consumed).max(0.0);
    let allowed_rate = 100.0 / 7.0;
    let burn_rate = nominal_consumed / elapsed_days;
    let pace_ratio = (burn_rate / allowed_rate) as f32;
    let runway_from_obs_days = nominal_rem_pct / burn_rate;
    let runway_days_f64 = (runway_from_obs_days - sample_age_days).max(0.0);
    let runway_days = runway_days_f64 as f32;
    let reserve_days_f64 = runway_from_obs_days - remaining_at_obs_days;
    let delta_days = Some(reserve_days_f64.round() as i32);

    let week_exhaustion_date = if runway_days < 30.0 {
        let today_days = ymd_to_days(today.0, today.1, today.2);
        let add_days = (runway_days + (now_hm.0 * 60 + now_hm.1) as f32 / 1440.0).floor() as i64;
        let (_y, ex_m, ex_d) = days_to_ymd(today_days + add_days);
        Some((ex_m, ex_d))
    } else {
        None
    };

    const EPSILON_DAYS: f64 = 1e-6;
    let (week_state, week_is_estimate, week_tight) = if let Some((low, high)) = week_consumed_range
    {
        let runway_from_now_at = |c: f32| -> f64 {
            let c64 = (c as f64).clamp(0.0, 100.0);
            if c64 <= 0.0 {
                f64::INFINITY
            } else {
                ((100.0 - c64).max(0.0) / (c64 / elapsed_days)) - sample_age_days
            }
        };
        let runway_max = runway_from_now_at(low);
        let runway_min = runway_from_now_at(high);
        if runway_max < remaining_days_f64 - EPSILON_DAYS {
            (WeekForecastState::ProvenDeficit, false, true)
        } else if runway_min > remaining_days_f64 + EPSILON_DAYS {
            (WeekForecastState::Active, false, false)
        } else {
            (WeekForecastState::EstimateNeutral, true, false)
        }
    } else {
        // Without a proven interval (both AppServer whole-percent and single
        // SessionLog observations), emit nominal estimates with neutral light.
        (WeekForecastState::EstimateNeutral, true, false)
    };

    let health = if week_tight || five_h_tight {
        PacingHealth::Tight
    } else if week_state == WeekForecastState::EstimateNeutral {
        PacingHealth::OnTrack
    } else if pace_ratio < 0.90 || nominal_rem_pct >= 80.0 {
        PacingHealth::Surplus
    } else {
        PacingHealth::OnTrack
    };

    let mut forecast = make_forecast(
        Some(pace_ratio),
        Some(runway_days),
        delta_days,
        remaining_days,
        health,
        week_exhaustion_date,
        week_state,
        week_is_estimate,
    );
    forecast.projected_week_reserve = Some(reserve_days_f64 as f32);
    forecast
}

#[derive(Clone, Debug)]
pub struct QuotaSnapshot {
    pub providers: Vec<ProviderQuota>,
    pub has_throttle: bool,
    #[allow(dead_code)]
    pub last_updated: Option<Instant>,
}

impl Default for QuotaSnapshot {
    fn default() -> Self {
        Self::measured_baseline()
    }
}

impl QuotaSnapshot {
    /// Initial display until live provider data is available.
    pub fn measured_baseline() -> Self {
        let providers = vec![
            ProviderQuota::new(
                ProviderId::Glm,
                "GLM",
                "Z.ai",
                Some(76),
                Some("16:41".into()),
                Some(75),
                Some("01.10.".into()),
            ),
            ProviderQuota::new(
                ProviderId::Agy,
                "AGY",
                "Antigravity",
                Some(98),
                Some("17:53".into()),
                Some(100),
                Some("25.09. (12:53)".into()),
            ),
            ProviderQuota::new(
                ProviderId::Codex,
                "Codex",
                "OpenAI",
                None,
                None,
                None,
                None,
            ),
            ProviderQuota::new(
                ProviderId::Claude,
                "Claude",
                "Anthropic",
                None,
                None,
                None,
                None,
            ),
            ProviderQuota::new(ProviderId::Qwen, "Qwen", "Alibaba", None, None, None, None),
        ];
        let has_throttle = providers.iter().any(|p| p.is_throttled);
        Self {
            providers,
            has_throttle,
            last_updated: Some(Instant::now()),
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, id: ProviderId) -> Option<&ProviderQuota> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Looks up one provider card by provider and account slot. Single-account
    /// providers carry `None` as account id.
    pub fn get_card(&self, id: ProviderId, account_id: Option<&str>) -> Option<&ProviderQuota> {
        self.providers
            .iter()
            .find(|p| p.id == id && p.account_id.as_deref() == account_id)
    }

    /// Global alarm flag: a throttled card counts only when its account is
    /// allowed to raise the alarm (see `multi_account_alarm_allowed`).
    fn alarm_throttle(&self) -> bool {
        self.providers
            .iter()
            .any(|p| p.is_throttled && p.multi_account_alarm_allowed(&self.providers))
    }
}

/// Sägezahn-Ampel für Mehrkonto-Pools: Reicht die Reichweite des aktiven
/// Kontos bis zum frühesten Wochen-Refill eines gesunden Partner-Kontos, ist
/// eine Tight-Warnung aus dem 7-Tage-Einzelhorizont ein Fehlalarm und wird
/// auf OnTrack begrenzt. Throttled, eine 5-h-Erschöpfung und die historische
/// Burnrate (`cycle_days`) bleiben unberührt.
fn apply_pool_refill_cap(fc: &mut PacingForecast, pool_next_refill_epoch: Option<i64>, now_epoch: i64) {
    let Some(refill_epoch) = pool_next_refill_epoch else {
        return;
    };
    if fc.health != PacingHealth::Tight {
        return;
    }
    if fc
        .five_hour_forecast
        .as_ref()
        .is_some_and(|five_hour| five_hour.is_exhausted_before_reset)
    {
        return;
    }
    let Some(runway_days) = fc.runway_days else {
        return;
    };
    let days_to_refill = (refill_epoch - now_epoch) as f32 / 86_400.0;
    if days_to_refill > 0.0 && runway_days >= days_to_refill {
        fc.health = PacingHealth::OnTrack;
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
    }
}

/// Helper to parse GLM JSON limit objects where percentage is CONSUMPTION.
/// Remaining = 100 - used.
#[allow(dead_code)]
pub fn parse_glm_limits(json_str: &str) -> Option<(Option<u8>, Option<u8>)> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let limits = value.get("data")?.get("limits")?.as_array()?;

    let mut five_hour = None;
    let mut week = None;

    for item in limits {
        let limit_type = item.get("type")?.as_str()?;
        let used_pct = item.get("percentage")?.as_f64()? as u8;
        let remaining_pct = 100u8.saturating_sub(used_pct);

        if limit_type == "TOKENS_LIMIT" {
            five_hour = Some(remaining_pct);
        } else if limit_type == "TIME_LIMIT" {
            week = Some(remaining_pct);
        }
    }

    Some((five_hour, week))
}

/// Helper to parse Agy TSV lines from `agy -p "/usage"`.
/// Lines format: ModelGroup \t LimitType \t Remaining% \t ResetIso
#[allow(dead_code)]
pub fn parse_agy_tsv(tsv_str: &str) -> Option<(Option<u8>, Option<u8>)> {
    let mut five_hour = None;
    let mut week = None;

    for line in tsv_str.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let limit_type = parts[1].trim();
        let pct_str = parts[2].trim().trim_end_matches('%');
        let pct = pct_str.parse::<u8>().ok()?;

        if limit_type.contains("Five Hour") {
            five_hour = Some(pct);
        } else if limit_type.contains("Weekly") {
            week = Some(pct);
        }
    }

    Some((five_hour, week))
}

/// Helper to parse Codex rate limit data where usedPercent is CONSUMPTION.
/// Remaining = 100 - used.
#[allow(dead_code)]
pub fn parse_codex_rate_limits(json_str: &str) -> Option<(Option<u8>, Option<u8>)> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let rate_limits = value.get("rateLimits")?;

    let week = rate_limits.get("primary")
        .and_then(|p| p.get("usedPercent"))
        .and_then(|u| u.as_f64())
        .map(|used| 100u8.saturating_sub(used as u8));

    let five_hour = rate_limits.get("secondary")
        .filter(|s| !s.is_null())
        .and_then(|s| s.get("usedPercent"))
        .and_then(|u| u.as_f64())
        .map(|used| 100u8.saturating_sub(used as u8));

    Some((five_hour, week))
}

/// Parses an ISO-8601 UTC timestamp of the form `YYYY-MM-DDTHH:MM[:SS]Z` into a Unix epoch.
pub fn iso_utc_to_epoch(iso_str: &str) -> Option<i64> {
    let raw = iso_str.trim().strip_suffix('Z')?;
    let (date, time) = raw.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = match time_parts.next() {
        Some(raw_second) => raw_second.split('.').next()?.parse().ok()?,
        None => 0,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(ymd_to_days(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Local `(month, day, hour, minute)` of a UTC epoch, shifted by the local offset.
fn utc_epoch_to_local_month_day_hm(epoch: i64) -> (u32, u32, u32, u32) {
    let local = epoch + local_timezone_offset_minutes() as i64 * 60;
    let (_year, month, day) = days_to_ymd(local.div_euclid(86_400));
    let day_seconds = local.rem_euclid(86_400);
    (month, day, (day_seconds / 3_600) as u32, ((day_seconds % 3_600) / 60) as u32)
}

/// Builds one AGY account card from an entry of the `accounts` array of the
/// `fetch_agy_quota.py` contract. Returns `None` for non-object entries.
fn agy_account_card(item: &serde_json::Value) -> Option<ProviderQuota> {
    if !item.is_object() {
        return None;
    }
    let text = |key: &str| {
        item.get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let percent = |key: &str| {
        item.get(key)
            .and_then(|value| value.as_f64())
            .map(|value| value.clamp(0.0, 100.0).round() as u8)
    };

    let five_hour_percent = percent("five_hour_pct");
    let week_percent = percent("week_pct");
    let five_hour_reset = text("five_hour_reset")
        .as_deref()
        .and_then(iso_utc_to_local_hm)
        .map(|(hour, minute)| format!("{hour:02}:{minute:02}"));
    let week_reset_raw = text("week_reset");
    let week_reset_epoch = week_reset_raw.as_deref().and_then(iso_utc_to_epoch);
    let week_reset = week_reset_epoch.map(|epoch| {
        let (month, day, hour, minute) = utc_epoch_to_local_month_day_hm(epoch);
        format!("{day}.{month}. ({hour:02}:{minute:02})")
    });

    let mut card = ProviderQuota::new(
        ProviderId::Agy,
        "AGY",
        "Antigravity",
        five_hour_percent,
        five_hour_reset,
        week_percent,
        week_reset,
    );
    card.account_id = text("id");
    card.account_label = text("label");
    card.account_email = text("email");
    // A missing flag counts as active: an unknown account must not hide a throttle alarm.
    card.is_active_account = item
        .get("is_active")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    // Without an explicit auth_error, a non-ok auth_state still blocks the account
    // (login pill, no switch click). Missing or "ok" states add no error.
    card.auth_error = text("auth_error").or_else(|| match text("auth_state").as_deref() {
        None | Some("ok") => None,
        Some("relogin_required") => Some("Login erneuern".to_string()),
        Some(other) => Some(other.to_string()),
    });
    card.week_reset_epoch = week_reset_epoch;
    Some(card)
}

/// Applies the `fetch_agy_quota.py` JSON contract (`version`, `active_account`,
/// `pooled_count`, `pool_next_refill_epoch`, `accounts`). The AGY cards are
/// replaced as a whole, so a pool that shrinks from two accounts to one leaves
/// no ghost card. `null`, invalid JSON or an empty account list change nothing.
pub fn update_snapshot_with_agy_json(snapshot: &mut QuotaSnapshot, json_str: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return;
    };
    let Some(accounts) = value.get("accounts").and_then(|items| items.as_array()) else {
        return;
    };
    let mut cards: Vec<ProviderQuota> = accounts.iter().filter_map(agy_account_card).collect();
    if cards.is_empty() {
        return;
    }

    let pooled_count = value
        .get("pooled_count")
        .and_then(|count| count.as_i64())
        .unwrap_or(cards.len() as i64);
    if pooled_count >= 2 {
        let pool_refill = value.get("pool_next_refill_epoch").and_then(|epoch| epoch.as_i64());
        if let (Some(refill), Some(active)) =
            (pool_refill, cards.iter_mut().find(|card| card.is_active_account))
        {
            if active.week_reset_epoch.is_some_and(|own_reset| refill < own_reset) {
                active.pool_next_refill_epoch = Some(refill);
            }
        }
    }

    // Everything before the first AGY entry is non-AGY, so its index stays valid after removal.
    let insert_at = snapshot
        .providers
        .iter()
        .position(|p| p.id == ProviderId::Agy)
        .unwrap_or(snapshot.providers.len());
    snapshot.providers.retain(|p| p.id != ProviderId::Agy);
    let insert_at = insert_at.min(snapshot.providers.len());
    drop(snapshot.providers.splice(insert_at..insert_at, cards.drain(..)));
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

#[allow(dead_code)]
pub fn update_snapshot_with_agy_tsv(snapshot: &mut QuotaSnapshot, tsv: &str) {
    if let Some(agy) = snapshot.providers.iter_mut().find(|p| p.id == ProviderId::Agy) {
        for line in tsv.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 4 {
                continue;
            }
            let group = parts[0].trim();
            let limit_type = parts[1].trim();
            let pct_str = parts[2].trim().trim_end_matches('%');
            let reset_raw = parts[3].trim();

            if group.contains("Gemini") {
                if let Ok(pct) = pct_str.parse::<u8>() {
                    if limit_type.contains("Five Hour") {
                        agy.five_hour_percent = Some(pct);
                        if let Some((h, m)) = iso_utc_to_local_hm(reset_raw) {
                            agy.five_hour_reset = Some(format!("{:02}:{:02}", h, m));
                        }
                    } else if limit_type.contains("Weekly") {
                        agy.week_percent = Some(pct);
                        let hm_opt = iso_utc_to_local_hm(reset_raw);
                        if let Some(date_part) = reset_raw.split('T').next() {
                            let date_pieces: Vec<&str> = date_part.split('-').collect();
                            if date_pieces.len() >= 3 {
                                let m = date_pieces[1];
                                let d = date_pieces[2];
                                if let Some((h, min)) = hm_opt {
                                    agy.week_reset = Some(format!("{d}.{m}. ({h:02}:{min:02})"));
                                } else {
                                    agy.week_reset = Some(format!("{d}.{m}."));
                                }
                            }
                        }
                    }
                }
            }
        }
        agy.is_throttled = agy.week_percent.unwrap_or(100) == 0 || agy.five_hour_percent.unwrap_or(100) == 0;
    }
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

pub fn update_snapshot_with_glm_json(snapshot: &mut QuotaSnapshot, json_str: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return;
    };
    let Some(limits) = value.get("data").and_then(|d| d.get("limits")).and_then(|l| l.as_array()) else {
        return;
    };

    if let Some(glm) = snapshot.providers.iter_mut().find(|p| p.id == ProviderId::Glm) {
        for item in limits {
            let Some(limit_type) = item.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            let used_pct = item.get("percentage").and_then(|p| p.as_f64()).map(|f| f as u8).unwrap_or(0);
            let remaining_pct = 100u8.saturating_sub(used_pct);
            let next_reset_ms = item.get("nextResetTime").and_then(|t| t.as_i64());

            if limit_type == "TOKENS_LIMIT" {
                glm.five_hour_percent = Some(remaining_pct);
                if let Some(ms) = next_reset_ms {
                    let secs = ms / 1000;
                    let offset_mins = local_timezone_offset_minutes() as i64;
                    let local_secs = secs + offset_mins * 60;
                    let day_secs = local_secs.rem_euclid(86400);
                    let h = (day_secs / 3600) as u32;
                    let m = ((day_secs % 3600) / 60) as u32;
                    glm.five_hour_reset = Some(format!("{h:02}:{m:02}"));
                }
            } else if limit_type == "TIME_LIMIT" {
                glm.week_percent = Some(remaining_pct);
                if let Some(ms) = next_reset_ms {
                    let secs = ms / 1000;
                    let offset_mins = local_timezone_offset_minutes() as i64;
                    let local_secs = secs + offset_mins * 60;
                    let days = local_secs / 86400;
                    let (_y, month, d) = days_to_ymd(days);
                    let day_secs = local_secs.rem_euclid(86400);
                    let h = (day_secs / 3600) as u32;
                    let min = ((day_secs % 3600) / 60) as u32;
                    glm.week_reset = Some(format!("{d:02}.{month:02}. ({h:02}:{min:02})"));
                }
            }
        }
        glm.is_throttled = glm.week_percent.unwrap_or(100) == 0 || glm.five_hour_percent.unwrap_or(100) == 0;
    }
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

#[cfg(windows)]
fn utc_epoch_to_local_parts(
    epoch: i64,
    time_zone_information: Option<&WinTimeZoneInformation>,
) -> Option<(u32, u32, u32, u32, u32)> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;

    let days = epoch.div_euclid(86_400);
    let day_seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = days_to_ymd(days);
    let universal_time = SYSTEMTIME {
        wYear: u16::try_from(year).ok()?,
        wMonth: month as u16,
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: (day_seconds / 3_600) as u16,
        wMinute: ((day_seconds % 3_600) / 60) as u16,
        wSecond: (day_seconds % 60) as u16,
        wMilliseconds: 0,
    };
    let mut local_time = SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let zone_ptr = time_zone_information
        .map_or(std::ptr::null(), |zone| zone as *const WinTimeZoneInformation);
    let converted = unsafe {
        SystemTimeToTzSpecificLocalTime(zone_ptr, &universal_time, &mut local_time)
    };
    if converted == 0 {
        return None;
    }
    Some((
        local_time.wYear as u32,
        local_time.wMonth as u32,
        local_time.wDay as u32,
        local_time.wHour as u32,
        local_time.wMinute as u32,
    ))
}

fn format_codex_reset_local(key: &str, parts: (u32, u32, u32, u32, u32)) -> String {
    let (_year, month, day, hour, minute) = parts;
    if key == "five_hour" {
        format!("{hour:02}:{minute:02}")
    } else {
        format!("{day:02}.{month:02}. ({hour:02}:{minute:02})")
    }
}

#[cfg(windows)]
fn codex_reset_epoch_to_local_with_timezone(
    epoch: i64,
    key: &str,
    time_zone_information: Option<&WinTimeZoneInformation>,
) -> String {
    if let Some(parts) = utc_epoch_to_local_parts(epoch, time_zone_information) {
        return format_codex_reset_local(key, parts);
    }

    // Keep a visible reset if Windows rejects an out-of-range timestamp.
    let local_secs = epoch + local_timezone_offset_minutes() as i64 * 60;
    let days = local_secs.div_euclid(86_400);
    let day_seconds = local_secs.rem_euclid(86_400);
    let (year, month, day) = days_to_ymd(days);
    format_codex_reset_local(
        key,
        (
            year as u32,
            month,
            day,
            (day_seconds / 3_600) as u32,
            ((day_seconds % 3_600) / 60) as u32,
        ),
    )
}

#[cfg(windows)]
fn codex_reset_epoch_to_local(epoch: i64, key: &str) -> String {
    codex_reset_epoch_to_local_with_timezone(epoch, key, None)
}

#[cfg(not(windows))]
fn codex_reset_epoch_to_local(epoch: i64, key: &str) -> String {
    // Non-Windows forecasts use UTC for both reset times and current time.
    let days = epoch.div_euclid(86_400);
    let day_seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = days_to_ymd(days);
    format_codex_reset_local(
        key,
        (
            year as u32,
            month,
            day,
            (day_seconds / 3_600) as u32,
            ((day_seconds % 3_600) / 60) as u32,
        ),
    )
}

fn parse_codex_source(window: &serde_json::Value) -> Option<CodexObservationSource> {
    match window.get("source").and_then(|v| v.as_str()) {
        Some("app_server") => Some(CodexObservationSource::AppServer),
        Some("session_log") => Some(CodexObservationSource::SessionLog),
        _ => None,
    }
}

fn is_week_exhaustion_confirmed(
    remaining: Option<u8>,
    consumed: Option<f32>,
    source: Option<CodexObservationSource>,
    explicit_week_throttled: bool,
) -> bool {
    if explicit_week_throttled {
        return true;
    }
    match source {
        // AppServer reports integer percentages without proven weekly lock
        // attribution when `week_throttled` is false; 100% is only a signal.
        Some(CodexObservationSource::AppServer) => false,
        Some(CodexObservationSource::SessionLog) => consumed.is_some_and(|c| c >= 100.0),
        None => remaining == Some(0) || consumed.is_some_and(|c| c >= 100.0),
    }
}

fn is_five_hour_exhaustion_confirmed(
    remaining: Option<u8>,
    consumed: Option<f32>,
    source: Option<CodexObservationSource>,
    explicit_five_hour_throttled: bool,
) -> bool {
    if explicit_five_hour_throttled {
        return true;
    }
    if source == Some(CodexObservationSource::SessionLog)
        && consumed.is_some_and(|c| c < 100.0)
    {
        return false;
    }
    remaining == Some(0) || consumed.is_some_and(|c| c >= 100.0)
}

pub fn update_snapshot_with_codex_json(snapshot: &mut QuotaSnapshot, json_str: &str) {
    let value = serde_json::from_str::<serde_json::Value>(json_str).ok();
    let codex = snapshot.providers.iter_mut().find(|p| p.id == ProviderId::Codex);
    let Some(codex) = codex else { return };

    // Every refresh replaces the observation. A missing or expired log must not
    // leave a previous cycle displayed as a current throttling event.
    codex.five_hour_percent = None;
    codex.five_hour_reset = None;
    codex.week_percent = None;
    codex.week_reset = None;
    codex.week_consumed_pct = None;
    codex.week_reset_epoch = None;
    codex.week_observed_epoch = None;
    codex.week_source = None;
    codex.week_consumed_range = None;
    codex.week_confirmed_exhausted = false;
    codex.five_hour_confirmed_exhausted = false;
    codex.five_hour_reset_epoch = None;
    codex.explicit_provider_throttle = false;

    let explicit_week_throttled = value
        .as_ref()
        .and_then(|v| v.get("week_throttled"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let explicit_five_hour_throttled = value
        .as_ref()
        .and_then(|v| v.get("five_hour_throttled"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    if let Some(window) = value.as_ref().and_then(|v| v.get("week")) {
        codex.week_percent = window
            .get("remaining_percent")
            .and_then(|p| p.as_u64())
            .filter(|p| *p <= 100)
            .map(|p| p as u8);
        codex.week_reset_epoch = window.get("reset_epoch").and_then(|v| v.as_i64());
        codex.week_reset = codex
            .week_reset_epoch
            .map(|epoch| codex_reset_epoch_to_local(epoch, "week"))
            .or_else(|| {
                window
                    .get("reset")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            });
        codex.week_consumed_pct = window
            .get("consumed_percent")
            .and_then(|v| v.as_f64())
            .filter(|c| (0.0..=100.0).contains(c))
            .map(|c| c as f32);
        codex.week_observed_epoch = window.get("observed_epoch").and_then(|v| v.as_i64());
        codex.week_source = parse_codex_source(window);
        codex.week_consumed_range = window
            .get("consumed_range")
            .and_then(|v| v.as_array())
            .filter(|arr| arr.len() == 2)
            .and_then(|arr| Some((arr[0].as_f64()? as f32, arr[1].as_f64()? as f32)))
            .filter(|(low, high)| (0.0..=100.0).contains(low) && (0.0..=100.0).contains(high) && low <= high);
        codex.week_confirmed_exhausted = is_week_exhaustion_confirmed(
            codex.week_percent,
            codex.week_consumed_pct,
            codex.week_source,
            explicit_week_throttled,
        );
    }

    if let Some(window) = value.as_ref().and_then(|v| v.get("five_hour")) {
        codex.five_hour_percent = window
            .get("remaining_percent")
            .and_then(|p| p.as_u64())
            .filter(|p| *p <= 100)
            .map(|p| p as u8);
        codex.five_hour_reset_epoch = window.get("reset_epoch").and_then(|v| v.as_i64());
        codex.five_hour_reset = codex
            .five_hour_reset_epoch
            .map(|epoch| codex_reset_epoch_to_local(epoch, "five_hour"))
            .or_else(|| {
                window
                    .get("reset")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            });
        let five_hour_consumed = window
            .get("consumed_percent")
            .and_then(|v| v.as_f64())
            .filter(|c| (0.0..=100.0).contains(c))
            .map(|c| c as f32);
        let five_hour_source = parse_codex_source(window);
        codex.five_hour_confirmed_exhausted = is_five_hour_exhaustion_confirmed(
            codex.five_hour_percent,
            five_hour_consumed,
            five_hour_source,
            explicit_five_hour_throttled,
        );
    }

    let explicit_provider_throttle = value
        .as_ref()
        .and_then(|v| v.get("is_throttled").or_else(|| v.get("throttled")))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    codex.explicit_provider_throttle =
        explicit_provider_throttle && !explicit_week_throttled && !explicit_five_hour_throttled;
    codex.is_throttled = codex.week_confirmed_exhausted
        || codex.five_hour_confirmed_exhausted
        || codex.explicit_provider_throttle;
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

pub fn update_snapshot_with_claude_json(snapshot: &mut QuotaSnapshot, json_str: &str) {
    let value = serde_json::from_str::<serde_json::Value>(json_str).ok();
    let claude = snapshot
        .providers
        .iter_mut()
        .find(|p| p.id == ProviderId::Claude);
    let Some(claude) = claude else { return };

    // A missing or stale capture must not leave an older quota looking current.
    claude.five_hour_percent = None;
    claude.five_hour_reset = None;
    claude.week_percent = None;
    claude.week_reset = None;

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    for (key, percent, reset) in [
        (
            "five_hour",
            &mut claude.five_hour_percent,
            &mut claude.five_hour_reset,
        ),
        ("week", &mut claude.week_percent, &mut claude.week_reset),
    ] {
        let Some(window) = value.as_ref().and_then(|value| value.get(key)) else {
            continue;
        };
        *percent = window
            .get("used_percentage")
            .and_then(serde_json::Value::as_f64)
            .filter(|used| (0.0..=100.0).contains(used))
            .map(|used| (100.0 - used.round()).clamp(0.0, 100.0) as u8);
        *reset = window
            .get("resets_at")
            .and_then(serde_json::Value::as_i64)
            .filter(|epoch| *epoch > now_epoch)
            .map(|epoch| codex_reset_epoch_to_local(epoch, key));
    }
    claude.is_throttled = claude.week_percent == Some(0) || claude.five_hour_percent == Some(0);
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

/// Qwen payloads already carry remaining percentages (the fetcher converts
/// consumed fractions). The large window arrives either as `month` (Token
/// Plan monthly credits) or as `week` (rolling weekly usage); whichever key
/// the live payload reports decides the card's cycle label.
fn read_qwen_window(
    value: Option<&serde_json::Value>,
    key: &str,
    now_epoch: i64,
) -> Option<(Option<u8>, Option<String>, Option<f32>, Option<f32>)> {
    let window = value?.get(key)?;
    let percent = window
        .get("remaining_percent")
        .and_then(serde_json::Value::as_u64)
        .filter(|percent| *percent <= 100)
        .map(|percent| percent as u8);
    let reset = window
        .get("reset_epoch")
        .and_then(serde_json::Value::as_i64)
        .filter(|epoch| *epoch > now_epoch)
        .map(|epoch| {
            codex_reset_epoch_to_local(
                epoch,
                if key == "five_hour" {
                    "five_hour"
                } else {
                    "week"
                },
            )
        })
        .or_else(|| {
            window
                .get("reset")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    // The fetcher attaches the exact cycle length from the subscription
    // window; without it the forecast must not guess the cycle age.
    let cycle_days = window
        .get("cycle_days")
        .and_then(serde_json::Value::as_f64)
        .filter(|days| (0.5..=400.0).contains(days))
        .map(|days| days as f32);
    let consumed = window
        .get("consumed_percent")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| (0.0..=100.0).contains(value))
        .map(|value| value as f32);
    if percent.is_none() && reset.is_none() {
        return None;
    }
    Some((percent, reset, cycle_days, consumed))
}

pub fn update_snapshot_with_qwen_json(snapshot: &mut QuotaSnapshot, json_str: &str) {
    let value = serde_json::from_str::<serde_json::Value>(json_str).ok();
    let qwen = snapshot
        .providers
        .iter_mut()
        .find(|p| p.id == ProviderId::Qwen);
    let Some(qwen) = qwen else { return };

    // A missing or stale capture must not leave an older quota looking current.
    qwen.five_hour_percent = None;
    qwen.five_hour_reset = None;
    qwen.week_percent = None;
    qwen.week_reset = None;
    qwen.cycle_days = None;
    qwen.week_consumed_pct = None;

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let payload = value.as_ref();
    if let Some((percent, reset, _, _)) = read_qwen_window(payload, "five_hour", now_epoch) {
        qwen.five_hour_percent = percent;
        qwen.five_hour_reset = reset;
    }
    // The Token Plan reports monthly credits; a rolling week window arrives
    // only as fallback. Whichever key carries data decides the cycle label.
    let month_window = read_qwen_window(payload, "month", now_epoch);
    let week_window = read_qwen_window(payload, "week", now_epoch);
    let large_window_is_month = month_window.is_some();
    if let Some((percent, reset, cycle_days, consumed)) = month_window.or(week_window) {
        qwen.month_cycle = large_window_is_month;
        qwen.week_percent = percent;
        qwen.week_reset = reset;
        qwen.cycle_days = cycle_days;
        qwen.week_consumed_pct = consumed;
    }
    qwen.is_throttled = qwen.week_percent == Some(0) || qwen.five_hour_percent == Some(0);
    snapshot.has_throttle = snapshot.alarm_throttle();
    snapshot.last_updated = Some(Instant::now());
}

#[cfg(windows)]
pub fn fetch_live_snapshot(current: &QuotaSnapshot, show_qwen: bool) -> QuotaSnapshot {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut updated = current.clone();

    let distro = crate::configuration::load().distro;

    // 1. Fetch AGY accounts. The fetcher owns the llm-account contract and
    // falls back to `agy -p /usage` itself when no account store exists.
    let agy_json = Command::new("wsl.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(&distro)
        .arg("--exec")
        .arg("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_agy_quota.py"))
        .output()
        .ok()
        .filter(|output| output.status.success());
    update_snapshot_with_agy_json(
        &mut updated,
        agy_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );

    // 2. Fetch GLM
    let mut glm_cmd = Command::new("wsl.exe");
    glm_cmd.creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(&distro)
        .arg("--exec")
        .arg("/home/simon/projects/herdr-night-watch/tools/fetch_glm.sh");

    if let Ok(output) = glm_cmd.output() {
        if output.status.success() {
            let json = String::from_utf8_lossy(&output.stdout);
            update_snapshot_with_glm_json(&mut updated, &json);
        }
    }

    let mut codex_cmd = Command::new("wsl.exe");
    codex_cmd.creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(&distro)
        .arg("--exec")
        .arg("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_codex_quota.py"));
    let codex_json = codex_cmd.output().ok().filter(|output| output.status.success());
    update_snapshot_with_codex_json(
        &mut updated,
        codex_json.as_ref().map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok()).unwrap_or("null"),
    );

    let claude_json = Command::new("wsl.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(&distro)
        .arg("--exec")
        .arg("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_claude_quota.py"))
        .arg("--read")
        .output()
        .ok()
        .filter(|output| output.status.success());
    update_snapshot_with_claude_json(
        &mut updated,
        claude_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );

    // The inference key cannot query the QwenCloud billing gateway, so the
    // fetcher uses console cookies and fails closed (no data, no card churn)
    // when they are missing. Hidden providers skip the network entirely.
    let qwen_json = if show_qwen {
        Command::new("wsl.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .arg("-d")
            .arg(&distro)
            .arg("--exec")
            .arg("python3")
            .arg("-c")
            .arg(include_str!("../tools/fetch_qwen_quota.py"))
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    } else {
        None
    };
    update_snapshot_with_qwen_json(
        &mut updated,
        qwen_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );

    updated
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn fetch_live_snapshot(current: &QuotaSnapshot, show_qwen: bool) -> QuotaSnapshot {
    let mut updated = current.clone();
    let agy_json = std::process::Command::new("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_agy_quota.py"))
        .output()
        .ok()
        .filter(|output| output.status.success());
    update_snapshot_with_agy_json(
        &mut updated,
        agy_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );
    if let Ok(output) = std::process::Command::new("/home/simon/projects/herdr-night-watch/tools/fetch_glm.sh")
        .output()
    {
        if output.status.success() {
            let json = String::from_utf8_lossy(&output.stdout);
            update_snapshot_with_glm_json(&mut updated, &json);
        }
    }
    let codex_json = std::process::Command::new("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_codex_quota.py"))
        .output().ok().filter(|output| output.status.success());
    update_snapshot_with_codex_json(
        &mut updated,
        codex_json.as_ref().map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok()).unwrap_or("null"),
    );
    let claude_json = std::process::Command::new("python3")
        .arg("-c")
        .arg(include_str!("../tools/fetch_claude_quota.py"))
        .arg("--read")
        .output()
        .ok()
        .filter(|output| output.status.success());
    update_snapshot_with_claude_json(
        &mut updated,
        claude_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );
    let qwen_json = if show_qwen {
        std::process::Command::new("python3")
            .arg("-c")
            .arg(include_str!("../tools/fetch_qwen_quota.py"))
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    } else {
        None
    };
    update_snapshot_with_qwen_json(
        &mut updated,
        qwen_json
            .as_ref()
            .map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or("null"),
    );
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_snapshot_baseline() {
        let snapshot = QuotaSnapshot::measured_baseline();
        assert_eq!(snapshot.providers.len(), 5);
        assert!(!snapshot.has_throttle);

        let qwen = snapshot.get(ProviderId::Qwen).expect("Qwen present");
        assert_eq!(qwen.title, "Qwen");
        assert_eq!(qwen.author, "Alibaba");
        assert_eq!(qwen.five_hour_percent, None);
        assert_eq!(qwen.week_percent, None);
        assert!(!qwen.is_throttled);

        let glm = snapshot.get(ProviderId::Glm).expect("GLM present");
        assert_eq!(glm.five_hour_percent, Some(76));
        assert_eq!(glm.week_percent, Some(75));
        assert!(!glm.is_throttled);

        let codex = snapshot.get(ProviderId::Codex).expect("Codex present");
        assert_eq!(codex.week_percent, None);
        assert_eq!(codex.five_hour_percent, None);
        assert!(!codex.is_throttled);
        assert_eq!(codex.week_reset, None);

        let claude = snapshot.get(ProviderId::Claude).expect("Claude present");
        assert_eq!(claude.week_percent, None);
        assert_eq!(claude.five_hour_percent, None);
        assert!(!claude.is_throttled);
        assert_eq!(claude.title, "Claude");
        assert_eq!(claude.author, "Anthropic");
        let claude_fc = claude.pacing_forecast();
        assert_eq!(claude_fc.localized_badge_text(Language::German), "Bereit");
        assert_eq!(claude_fc.localized_summary_text(Language::German), "Telemetrie ausstehend");
    }

    #[test]
    fn exhausted_week_disables_the_whole_provider_card() {
        // Simon's case: weekly budget at zero while the session window shows
        // 100% - the session limit is worthless without weekly budget, so the
        // card must render fully disabled (is_throttled drives both blocks).
        let claude = ProviderQuota::new(
            ProviderId::Claude,
            "Claude",
            "Anthropic",
            Some(100),
            Some("22:00".into()),
            Some(0),
            Some("30.10.".into()),
        );
        assert!(claude.is_throttled);

        // Session at zero alone also disables the provider.
        let agy = ProviderQuota::new(
            ProviderId::Agy,
            "AGY",
            "Antigravity",
            Some(0),
            Some("19:45".into()),
            Some(73),
            Some("137h".into()),
        );
        assert!(agy.is_throttled);

        // Healthy budgets stay active.
        let glm = ProviderQuota::new(
            ProviderId::Glm,
            "GLM",
            "Z.ai",
            Some(90),
            Some("16:41".into()),
            Some(85),
            Some("01.10.".into()),
        );
        assert!(!glm.is_throttled);
    }

    #[test]
    fn claude_rate_limits_convert_used_percent_to_remaining_percent() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_claude_json(
            &mut snapshot,
            r#"{
                "five_hour": {"used_percentage": 32.4, "resets_at": 2000000000},
                "week": {"used_percentage": 4, "resets_at": 2000100000}
            }"#,
        );

        let claude = snapshot.get(ProviderId::Claude).expect("Claude present");
        assert_eq!(claude.five_hour_percent, Some(68));
        assert_eq!(claude.week_percent, Some(96));
        assert!(claude.five_hour_reset.is_some());
        assert!(claude.week_reset.is_some());
        assert!(!claude.is_throttled);
    }

    #[test]
    fn missing_claude_rate_limits_clear_old_values_instead_of_showing_unlimited() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_claude_json(
            &mut snapshot,
            r#"{"five_hour":{"used_percentage":0},"week":{"used_percentage":5}}"#,
        );
        assert_eq!(
            snapshot.get(ProviderId::Claude).unwrap().five_hour_percent,
            Some(100)
        );

        update_snapshot_with_claude_json(&mut snapshot, "null");

        let claude = snapshot.get(ProviderId::Claude).expect("Claude present");
        assert_eq!(claude.five_hour_percent, None);
        assert_eq!(claude.week_percent, None);
        assert_eq!(claude.five_hour_reset, None);
        assert_eq!(claude.week_reset, None);
        assert!(!claude.is_throttled);
    }

    #[test]
    fn provider_count_matches_the_baseline_card_set() {
        assert_eq!(PROVIDER_COUNT, QuotaSnapshot::measured_baseline().providers.len());
    }

    #[cfg(windows)]
    #[test]
    fn provider_count_matches_the_default_order() {
        assert_eq!(PROVIDER_COUNT, crate::window_settings::default_quota_order().len());
    }

    #[test]
    fn protection_shield_softens_small_deficits_on_rich_cards() {
        // Simon's case: 98 % left, pace 1.08x, -2 d projection must read as a
        // yellow warning, not a red alert; AGY's 5 % card stays unshielded.
        let fc = PacingForecast {
            pace_ratio: Some(1.08),
            runway_days: Some(28.0),
            delta_days: Some(-2),
            remaining_days: 30.3,
            health: PacingHealth::Surplus,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: None,
            week_exhaustion_date: Some((12, 3)),
            reset_str: Some("05.12. (17:00)".into()),
            today: (2026, 11, 5),
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        assert!(fc.week_is_deficit());
        assert!(fc.week_deficit_shielded(98));
        assert!(!fc.week_deficit_shielded(5));
        let fast = PacingForecast {
            pace_ratio: Some(1.6),
            ..fc.clone()
        };
        assert!(!fast.week_deficit_shielded(98));
    }

    #[test]
    fn qwen_month_window_wins_and_sets_the_monthly_cycle_label() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_qwen_json(
            &mut snapshot,
            r#"{
                "five_hour": {"remaining_percent": 84, "reset_epoch": 2000000000},
                "month": {"remaining_percent": 61, "reset_epoch": 2000500000}
            }"#,
        );
        let qwen = snapshot.get(ProviderId::Qwen).expect("Qwen present");
        assert_eq!(qwen.five_hour_percent, Some(84));
        assert_eq!(qwen.week_percent, Some(61));
        assert!(qwen.month_cycle);
        assert_eq!(qwen.cycle_label(), "Mo");
    }

    #[test]
    fn qwen_payload_carries_the_exact_subscription_cycle_length() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_qwen_json(
            &mut snapshot,
            r#"{
                "month": {"remaining_percent": 61, "reset_epoch": 2000500000, "cycle_days": 31.0, "consumed_percent": 38.75}
            }"#,
        );
        let qwen = snapshot.get(ProviderId::Qwen).expect("Qwen present");
        assert_eq!(qwen.cycle_days, Some(31.0));
        assert_eq!(qwen.week_consumed_pct, Some(38.75));

        // A payload without the cycle hint must clear it, not keep the old one.
        update_snapshot_with_qwen_json(
            &mut snapshot,
            r#"{"month": {"remaining_percent": 60, "reset_epoch": 2000500000}}"#,
        );
        assert_eq!(snapshot.get(ProviderId::Qwen).unwrap().cycle_days, None);
    }

    #[test]
    fn qwen_week_only_payload_falls_back_to_the_weekly_cycle() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_qwen_json(
            &mut snapshot,
            r#"{
                "five_hour": {"remaining_percent": 90, "reset_epoch": 2000000000},
                "week": {"remaining_percent": 70, "reset_epoch": 2000500000}
            }"#,
        );
        let qwen = snapshot.get(ProviderId::Qwen).expect("Qwen present");
        assert_eq!(qwen.week_percent, Some(70));
        assert!(!qwen.month_cycle);
        assert_eq!(qwen.cycle_label(), "Wk");
    }

    #[test]
    fn fresh_cycle_without_known_length_shows_no_fabricated_pace() {
        // Simon's case: a three-hour-old monthly window guessed as 30 days
        // produced "Pace: 432.00x · -31d Defizit". Without the exact cycle
        // length the forecast must stay silent instead.
        let fc = pacing_forecast_with_cycle_days(
            false,
            Some(99),
            Some("05.12. (17:00)"),
            None,
            None,
            None,
            None,
            Some((2026, 11, 4)),
            Some((21, 0)),
        );
        assert_eq!(fc.pace_ratio, None);
        assert_eq!(fc.runway_days, None);
    }

    #[test]
    fn fresh_cycle_stays_silent_even_with_exact_length() {
        // Four hours into a 31-day cycle at 1 % consumed: too little
        // telemetry for any runway or deficit claim - the card must show
        // no pace instead of "-22d Defizit".
        let fc = pacing_forecast_with_cycle_days(
            false,
            Some(99),
            Some("05.12. (17:00)"),
            None,
            None,
            Some(31.0),
            Some(1.43),
            Some((2026, 11, 4)),
            Some((21, 0)),
        );
        assert_eq!(fc.pace_ratio, None);
        assert_eq!(fc.runway_days, None);
        assert_eq!(fc.delta_days, None);
    }

    #[test]
    fn aged_cycle_with_exact_length_yields_sane_pace() {
        // Ten days into a 31-day cycle at 25 % consumed: burn 2.5 %/day vs
        // allowed 3.23 %/day -> pace below 1, no deficit.
        let fc = pacing_forecast_with_cycle_days(
            false,
            Some(75),
            Some("25.11. (17:00)"),
            None,
            None,
            Some(31.0),
            None,
            Some((2026, 11, 4)),
            Some((21, 0)),
        );
        let pace = fc.pace_ratio.expect("aged cycle carries a real pace");
        assert!(pace < 1.2, "pace should track the plan, got {pace}");
    }

    #[test]
    fn fresh_cycle_with_heavy_consumed_still_warns() {
        // Two hours into the cycle but 20 % consumed: above the rounding
        // noise band, so the forecast must speak up.
        let fc = pacing_forecast_with_cycle_days(
            false,
            Some(80),
            Some("05.12. (17:00)"),
            None,
            None,
            Some(31.0),
            Some(20.0),
            Some((2026, 11, 4)),
            Some((21, 0)),
        );
        assert!(fc.pace_ratio.is_some());
    }

    #[test]
    fn missing_qwen_payload_clears_old_values_instead_of_showing_stale_quota() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_qwen_json(
            &mut snapshot,
            r#"{"five_hour": {"remaining_percent": 40, "reset_epoch": 2000000000}}"#,
        );
        assert_eq!(
            snapshot.get(ProviderId::Qwen).unwrap().five_hour_percent,
            Some(40)
        );

        update_snapshot_with_qwen_json(&mut snapshot, "null");
        let qwen = snapshot.get(ProviderId::Qwen).expect("Qwen present");
        assert_eq!(qwen.five_hour_percent, None);
        assert_eq!(qwen.week_percent, None);
        assert_eq!(qwen.five_hour_reset, None);
        assert_eq!(qwen.week_reset, None);
        assert!(!qwen.is_throttled);
    }

    #[test]
    fn test_parse_glm_limits() {
        let json = r#"{
            "data": {
                "limits": [
                    {"type": "TOKENS_LIMIT", "percentage": 24.0},
                    {"type": "TIME_LIMIT", "percentage": 25.0}
                ]
            }
        }"#;
        let (five_h, wk) = parse_glm_limits(json).expect("valid parse");
        assert_eq!(five_h, Some(76));
        assert_eq!(wk, Some(75));
    }

    #[test]
    fn test_parse_agy_tsv() {
        let tsv = "Gemini Models\tFive Hour Limit Remaining\t98%\t2026-09-18T15:53:00Z\nGemini Models\tWeekly Limit Remaining\t100%\t2026-09-25T10:53:00Z";
        let (five_h, wk) = parse_agy_tsv(tsv).expect("valid parse");
        assert_eq!(five_h, Some(98));
        assert_eq!(wk, Some(100));
    }

    #[test]
    fn test_parse_codex_rate_limits() {
        let json = r#"{
            "rateLimits": {
                "primary": {"usedPercent": 100.0, "windowDurationMins": 10080},
                "secondary": null
            }
        }"#;
        let (five_h, wk) = parse_codex_rate_limits(json).expect("valid parse");
        assert_eq!(five_h, None);
        assert_eq!(wk, Some(0));
    }

    #[test]
    fn test_codex_live_observation_replaces_expired_cycle() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_codex_json(
            &mut snapshot,
            r#"{"week":{"remaining_percent":98,"reset_epoch":1790790254},"five_hour":{"remaining_percent":0,"reset_epoch":1790190000}}"#,
        );
        let codex = snapshot.get(ProviderId::Codex).unwrap();
        assert_eq!(codex.week_percent, Some(98));
        assert_eq!(codex.five_hour_percent, Some(0));
        assert_eq!(
            codex.week_reset,
            Some(codex_reset_epoch_to_local(1790790254, "week"))
        );
        assert_eq!(
            codex.five_hour_reset,
            Some(codex_reset_epoch_to_local(1790190000, "five_hour"))
        );
        assert!(snapshot.has_throttle);
        assert!(codex.codex_pacing_forecast_at(1790189999, (2026, 9, 23), (18, 0)).five_hour_forecast.is_some());

        update_snapshot_with_codex_json(&mut snapshot, "null");
        let codex = snapshot.get(ProviderId::Codex).unwrap();
        assert_eq!(codex.week_percent, None);
        assert_eq!(codex.five_hour_percent, None);
        assert_eq!(codex.week_reset, None);
        assert!(!snapshot.has_throttle);
    }

    #[cfg(windows)]
    #[test]
    fn codex_reset_and_week_forecast_follow_the_timestamp_timezone_rules() {
        use windows_sys::Win32::Foundation::SYSTEMTIME;

        let transition = |month, hour| SYSTEMTIME {
            wYear: 0,
            wMonth: month,
            wDayOfWeek: 0,
            wDay: 5,
            wHour: hour,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        let berlin = WinTimeZoneInformation {
            Bias: -60,
            StandardName: [0; 32],
            StandardDate: transition(10, 3),
            StandardBias: 0,
            DaylightName: [0; 32],
            DaylightDate: transition(3, 2),
            DaylightBias: -60,
        };

        assert_eq!(
            codex_reset_epoch_to_local_with_timezone(
                1_792_891_800,
                "five_hour",
                Some(&berlin),
            ),
            "02:30"
        );
        assert_eq!(
            codex_reset_epoch_to_local_with_timezone(
                1_774_747_800,
                "five_hour",
                Some(&berlin),
            ),
            "03:30"
        );

        let local_after_utc_midnight =
            utc_epoch_to_local_parts(1_790_116_200, Some(&berlin)).unwrap();
        assert_eq!(local_after_utc_midnight, (2026, 9, 23, 0, 30));
        let forecast = pacing_forecast_for(
            false,
            Some(98),
            Some("30.09. (12:00)"),
            None,
            None,
            Some((
                local_after_utc_midnight.0 as i32,
                local_after_utc_midnight.1,
                local_after_utc_midnight.2,
            )),
            Some((local_after_utc_midnight.3, local_after_utc_midnight.4)),
        );
        assert!((forecast.remaining_days - (7.0 + 11.5 / 24.0)).abs() < 0.0001);

        let (year, month, day, _, _, _) = current_local_datetime();
        assert_eq!(current_forecast_today(), (year as i32, month, day));
    }

    #[test]
    fn glm_satellite_lines_light_correctly_2026_11_10() {
        // Live satellite 10.11.2026 12:06: run the real snapshot update, then
        // the real extracted lights for both rendered lines.
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_glm_json(
            &mut snapshot,
            r#"{"code":200,"data":{"limits":[
                {"type":"TIME_LIMIT","unit":5,"number":1,"usage":4000,"currentValue":21,"remaining":3979,"percentage":1,"nextResetTime":1796139274996},
                {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":89,"nextResetTime":1794310620069}
            ]},"success":true}"#,
        );
        let glm = snapshot.get(ProviderId::Glm).expect("GLM present");
        let mut fc = pacing_forecast_with_cycle_days(
            glm.is_throttled,
            glm.week_percent,
            glm.week_reset.as_deref(),
            glm.five_hour_percent,
            glm.five_hour_reset.as_deref(),
            glm.cycle_days,
            glm.week_consumed_pct,
            Some((2026, 11, 10)),
            Some((12, 6)),
        );
        // The five-hour forecast is time-of-day based; pin it to the capture
        // (11 % remaining, reset 12:37 at 12:06) regardless of host timezone.
        fc.five_hour_forecast = calculate_five_hour_forecast(Some(11), Some("12:37"), (12, 6));
        let month = runway_light(glm.is_throttled, glm.week_percent.unwrap_or(100), false, &fc);
        let five = runway_light(glm.is_throttled, glm.five_hour_percent.unwrap_or(100), true, &fc);
        assert_eq!(
            month.color,
            RunwayColor::Green,
            "month line at 99 % with >30 d reserve must be green, diff={:?}",
            fc.runway_days.map(|rw| rw - fc.remaining_days)
        );
        assert_eq!(
            five.color,
            RunwayColor::Yellow,
            "five-hour line at 11 % remaining stays a low-balance warning"
        );
    }

    #[test]
    fn runway_light_keeps_week_and_five_hour_scenarios() {
        let mut fc = PacingForecast {
            pace_ratio: Some(1.19),
            runway_days: Some(2.4),
            delta_days: Some(-1),
            remaining_days: 3.5,
            health: PacingHealth::Tight,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: None,
            week_exhaustion_date: Some((12, 11)),
            reset_str: None,
            today: (2026, 11, 10),
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        // AGY weekly: severe deficit, 41 % remaining is under the 50 % shield.
        assert_eq!(runway_light(false, 41, false, &fc).color, RunwayColor::Red);
        // Codex weekly: 17 % remaining with a positive buffer renders green.
        fc.pace_ratio = Some(0.96);
        fc.runway_days = Some(1.3);
        fc.remaining_days = 1.0;
        assert_eq!(runway_light(false, 17, false, &fc).color, RunwayColor::Green);
        // AGY five-hour: 98 % remaining, runway outlives the reset.
        fc.five_hour_forecast = Some(FiveHourForecast {
            burn_rate: 0.1,
            pace_ratio: 0.6,
            runway_minutes: Some(1200),
            delta_minutes: Some(191),
            exhaustion_time: Some((20, 40)),
            reset_time: (17, 29),
            is_exhausted_before_reset: false,
        });
        assert_eq!(runway_light(false, 98, true, &fc).color, RunwayColor::Green);
        // Throttled provider: gray everywhere.
        assert_eq!(runway_light(true, 0, false, &fc).color, RunwayColor::Gray);
        assert_eq!(pace_light(true, 0, &runway_light(true, 0, false, &fc), Some(0.5)), RunwayColor::Gray);
    }

    #[test]
    fn runway_light_boundary_flags_match_thresholds() {
        // CodeRabbit inline 4524790892: pct >= 50 with a diff between -0.85
        // and -0.04 must be yellow with ONLY the yellow flag set; pct < 10
        // with diff == -0.85 must be red with ONLY the red flag set.
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: Some(4.5),
            delta_days: Some(0),
            remaining_days: 5.0,
            health: PacingHealth::Tight,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: None,
            week_exhaustion_date: None,
            reset_str: None,
            today: (2026, 11, 10),
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        let moderate = runway_light(false, 80, false, &fc); // diff = -0.5
        assert_eq!(moderate.color, RunwayColor::Yellow);
        assert!(!moderate.red && moderate.yellow);

        fc.runway_days = Some(4.15); // diff = -0.85
        fc.pace_ratio = Some(2.0); // defeats the shield at pct < 50 anyway
        let edge = runway_light(false, 9, false, &fc);
        assert_eq!(edge.color, RunwayColor::Red);
        assert!(edge.red && !edge.yellow);
    }

    #[test]
    fn pace_light_boundary_matches_ratio_thresholds() {
        // CodeRabbit inline 4524790907: with a clean (green) runway and
        // pct 100, pace None is gray, 1.0 and 1.25 are yellow; the
        // implementation stays untouched.
        let mut fc = PacingForecast {
            pace_ratio: None,
            runway_days: Some(900.0),
            delta_days: Some(0),
            remaining_days: 21.0,
            health: PacingHealth::Surplus,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast: None,
            week_exhaustion_date: None,
            reset_str: None,
            today: (2026, 11, 10),
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        };
        let green = runway_light(false, 100, false, &fc);
        assert_eq!(green.color, RunwayColor::Green);
        assert_eq!(pace_light(false, 100, &green, None), RunwayColor::Gray);
        assert_eq!(pace_light(false, 100, &green, Some(1.0)), RunwayColor::Yellow);
        fc.pace_ratio = Some(1.25);
        let green2 = runway_light(false, 100, false, &fc);
        assert_eq!(pace_light(false, 100, &green2, Some(1.25)), RunwayColor::Yellow);
    }

    #[test]
    fn test_pacing_forecast_baseline() {
        let today = (2026, 9, 18);
        let now = (14, 0);

        // GLM: 75% remaining, reset 01.10 (13 days away, 17 days elapsed of 30) -> Surplus
        // 5h: 76% remaining, reset 16:41 (161 mins away, 139 mins elapsed) -> lasts until 21:20 (+5 Stunden)
        let glm_fc = pacing_forecast_for(
            false,
            Some(75),
            Some("01.10."),
            Some(76),
            Some("16:41"),
            Some(today),
            Some(now),
        );
        assert_eq!(glm_fc.health, PacingHealth::Surplus);
        assert!(glm_fc.pace_ratio.unwrap() < 0.6);
        assert_eq!(glm_fc.badge_text, "+4h 39m Reserve (21:20)");
        assert_eq!(glm_fc.summary_text, "+>30 Tage Reserve");

        // AGI: 100% remaining, reset 25.09 (7 days away, 0 consumed) -> Surplus / buffer
        let agi_fc = pacing_forecast_for(
            false,
            Some(100),
            Some("25.09."),
            Some(98),
            Some("17:53"),
            Some(today),
            Some(now),
        );
        assert_eq!(agi_fc.health, PacingHealth::Surplus);
        assert_eq!(agi_fc.badge_text, "+>24h Reserve");
        assert_eq!(agi_fc.summary_text, "+>30 Tage Reserve");

        // Codex: Throttled (0% remaining, reset 23.09) -> Throttled
        let codex_fc = pacing_forecast_for(
            true,
            Some(0),
            Some("23.09."),
            None,
            None,
            Some(today),
            Some(now),
        );
        assert_eq!(codex_fc.health, PacingHealth::Throttled);
        assert_eq!(codex_fc.badge_text, "Gedrosselt");
        assert_eq!(codex_fc.summary_text, "Reset 23.09.");
    }

    #[test]
    fn test_pacing_forecast_simon_scenario_tight() {
        // Simon's scenario: 50% consumed on Day 2 of 7-day week (5 days remaining)
        // Reset in 5 days -> 2026-09-23 if today is 2026-09-18. Runway = 1.8 days -> -3 Tage (20.09.)
        let today = (2026, 9, 18);
        let now = (14, 0);
        let fc = pacing_forecast_for(
            false,
            Some(50),
            Some("23.09."),
            None,
            None,
            Some(today),
            Some(now),
        );
        assert_eq!(fc.health, PacingHealth::Tight);
        assert!(fc.pace_ratio.unwrap() >= 1.6);
        assert_eq!(fc.summary_text, "-3 Tage (20.09.)");
    }

    #[test]
    fn test_five_hour_forecast_burst_exhaustion() {
        // Rapid 5h burst: 70% consumed in 79 minutes (30% remaining, reset at 16:41, now 13:00)
        // Burn rate = 70 / 79 = 0.886%/min -> runway = 30 / 0.886 = 34 mins -> empty at 13:34
        // Delta from reset (221 mins away) = 34 - 221 = -187 mins -> -3 Stunden
        let today = (2026, 9, 18);
        let now = (13, 0);
        let fc = pacing_forecast_for(
            false,
            Some(80),
            Some("01.10."),
            Some(30),
            Some("16:41"),
            Some(today),
            Some(now),
        );
        assert_eq!(fc.health, PacingHealth::Tight);
        assert!(fc.five_hour_forecast.as_ref().unwrap().is_exhausted_before_reset);
        assert_eq!(fc.localized_upper_line(Language::German), "-3 Stunden (13:34)");
        assert_eq!(fc.localized_upper_line(Language::English), "-3 hours (13:34)");
    }

    #[test]
    fn test_pacing_forecast_localization_en_de() {
        let today = (2026, 9, 18);
        let now = (14, 0);

        // Throttled: DE = "Gedrosselt", EN = "Throttled"
        let codex_fc = pacing_forecast_for(
            true,
            Some(0),
            Some("23.09."),
            None,
            None,
            Some(today),
            Some(now),
        );
        assert_eq!(codex_fc.localized_badge_text(Language::German), "Gedrosselt");
        assert_eq!(codex_fc.localized_badge_text(Language::English), "Throttled");
        assert_eq!(codex_fc.localized_summary_text(Language::German), "Reset 23.09.");
        assert_eq!(codex_fc.localized_summary_text(Language::English), "Reset 23.09.");

        // GLM surplus: >14 days delta maps to +>30 Tage Reserve / +>30 days reserve
        let glm_fc = pacing_forecast_for(
            false,
            Some(75),
            Some("01.10."),
            Some(76),
            Some("16:41"),
            Some(today),
            Some(now),
        );
        assert_eq!(glm_fc.localized_upper_line(Language::German), "+4h 39m Reserve (21:20)");
        assert_eq!(glm_fc.localized_upper_line(Language::English), "+4h 39m reserve (21:20)");
        assert_eq!(glm_fc.localized_lower_line(Language::German), "+>30 Tage Reserve");
        assert_eq!(glm_fc.localized_lower_line(Language::English), "+>30 days reserve");

        // AGI surplus: minimal burn DE = "+>30 Tage Reserve", EN = "+>30 days reserve"
        let agi_fc = pacing_forecast_for(
            false,
            Some(100),
            Some("25.09."),
            Some(98),
            Some("17:53"),
            Some(today),
            Some(now),
        );
        assert_eq!(agi_fc.localized_upper_line(Language::German), "+>24h Reserve");
        assert_eq!(agi_fc.localized_upper_line(Language::English), "+>24h reserve");
        assert_eq!(agi_fc.localized_lower_line(Language::German), "+>30 Tage Reserve");
        assert_eq!(agi_fc.localized_lower_line(Language::English), "+>30 days reserve");

        // Tight: DE = "-3 Tage (20.09.)", EN = "-3 days (20.09)"
        let tight_fc = pacing_forecast_for(
            false,
            Some(50),
            Some("23.09."),
            None,
            None,
            Some(today),
            Some(now),
        );
        assert_eq!(tight_fc.localized_lower_line(Language::German), "-3 Tage (20.09.)");
        assert_eq!(tight_fc.localized_lower_line(Language::English), "-3 days (20.09)");
    }

    #[test]
    fn test_simon_glm_2218_past_cycle_scenario() {
        // Simon's exact scenario: current time is 22:18, GLM has 76% remaining, reset was 16:41.
        // 16:41 was 5.6 hours ago (>300 mins diff). It must NOT calculate false burst rate or trigger WARN!
        let today = (2026, 9, 18);
        let now = (22, 18);
        let fc = pacing_forecast_for(
            false,
            Some(75),
            Some("01.10."),
            Some(76),
            Some("16:41"),
            Some(today),
            Some(now),
        );
        assert_eq!(fc.health, PacingHealth::Surplus);
        assert_eq!(fc.localized_upper_line(Language::English), "+>24h reserve");
        assert_eq!(fc.localized_upper_line(Language::German), "+>24h Reserve");
        assert_eq!(fc.localized_lower_line(Language::English), "+>30 days reserve");
    }

    fn agy_account_json(id: &str, label: &str, is_active: bool, five: u8, week: u8, week_reset: &str) -> String {
        format!(
            r#"{{"id":"{id}","label":"{label}","email":"{label}@example.invalid","is_active":{is_active},"auth_state":"ok","five_hour_pct":{five},"five_hour_reset":null,"week_pct":{week},"week_reset":"{week_reset}"}}"#
        )
    }

    fn agy_pool_json(accounts: &[String], pooled_count: usize, refill: Option<i64>) -> String {
        let refill_json = refill.map_or("null".to_string(), |epoch| epoch.to_string());
        format!(
            r#"{{"version":1,"active_account":"acc2","pooled_count":{pooled_count},"pool_next_refill_epoch":{refill_json},"accounts":[{}]}}"#,
            accounts.join(",")
        )
    }

    fn agy_cards(snapshot: &QuotaSnapshot) -> Vec<&ProviderQuota> {
        snapshot.providers.iter().filter(|p| p.id == ProviderId::Agy).collect()
    }

    #[test]
    fn agy_json_builds_one_card_per_account_in_slot_order() {
        let json = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 0, 0, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 98, 99, "2026-11-27T08:08:12Z"),
            ],
            2,
            None,
        );
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_agy_json(&mut snapshot, &json);

        let cards = agy_cards(&snapshot);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].account_id.as_deref(), Some("acc1"));
        assert_eq!(cards[1].account_id.as_deref(), Some("acc2"));
        assert_eq!(cards[1].account_label.as_deref(), Some("work"));
        assert_eq!(cards[1].account_email.as_deref(), Some("work@example.invalid"));
        assert!(!cards[0].is_active_account);
        assert!(cards[1].is_active_account);
        assert_eq!(cards[1].week_percent, Some(99));
        assert_eq!(cards[1].five_hour_percent, Some(98));
        let card = snapshot.get_card(ProviderId::Agy, Some("acc2")).expect("acc2 card");
        assert_eq!(card.week_percent, Some(99));
        assert!(snapshot.get_card(ProviderId::Agy, None).is_none());
        // The other providers keep their positions around the AGY block.
        assert_eq!(snapshot.providers[0].id, ProviderId::Glm);
        assert_eq!(snapshot.providers[4].id, ProviderId::Claude);
    }

    #[test]
    fn agy_json_shrinking_pool_leaves_no_ghost_card() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let two = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 0, 0, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 98, 99, "2026-11-27T08:08:12Z"),
            ],
            2,
            None,
        );
        update_snapshot_with_agy_json(&mut snapshot, &two);
        assert_eq!(agy_cards(&snapshot).len(), 2);

        let one = r#"{"version":1,"active_account":null,"pooled_count":1,"pool_next_refill_epoch":null,"accounts":[{"id":null,"label":null,"email":"simon@example.invalid","is_active":true,"auth_state":"ok","five_hour_pct":50,"five_hour_reset":null,"week_pct":60,"week_reset":"2026-11-27T08:08:12Z"}]}"#;
        update_snapshot_with_agy_json(&mut snapshot, one);
        let cards = agy_cards(&snapshot);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].account_id, None);
        assert_eq!(cards[0].week_percent, Some(60));
    }

    #[test]
    fn agy_json_null_or_invalid_keeps_the_previous_cards() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let json = agy_pool_json(
            &[agy_account_json("acc1", "personal", true, 40, 70, "2026-11-27T08:08:12Z")],
            1,
            None,
        );
        update_snapshot_with_agy_json(&mut snapshot, &json);
        let before = snapshot.providers.clone();

        update_snapshot_with_agy_json(&mut snapshot, "null");
        update_snapshot_with_agy_json(&mut snapshot, "not json at all");
        update_snapshot_with_agy_json(&mut snapshot, r#"{"accounts":[]}"#);
        update_snapshot_with_agy_json(&mut snapshot, r#"{"version":1}"#);
        assert_eq!(snapshot.providers, before);
    }

    #[test]
    fn exhausted_inactive_reserve_does_not_raise_the_alarm() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let json = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 0, 0, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 98, 99, "2026-11-27T08:08:12Z"),
            ],
            2,
            None,
        );
        update_snapshot_with_agy_json(&mut snapshot, &json);

        assert!(!snapshot.has_throttle);
        let reserve = snapshot.get_card(ProviderId::Agy, Some("acc1")).expect("acc1 card");
        assert!(reserve.provider_is_throttled(), "the reserve card still shows its depletion");
        assert!(!reserve.multi_account_alarm_allowed(&snapshot.providers));
    }

    #[test]
    fn exhausted_active_account_raises_the_alarm() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let json = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 80, 80, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 98, 0, "2026-11-27T08:08:12Z"),
            ],
            2,
            None,
        );
        update_snapshot_with_agy_json(&mut snapshot, &json);
        assert!(snapshot.has_throttle);
    }

    #[test]
    fn all_accounts_exhausted_raise_the_alarm() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let json = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 0, 0, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 0, 0, "2026-11-27T08:08:12Z"),
            ],
            2,
            None,
        );
        update_snapshot_with_agy_json(&mut snapshot, &json);
        assert!(snapshot.has_throttle);
    }

    #[test]
    fn pool_refill_is_recorded_only_before_the_active_weekly_reset() {
        let refill = 1_795_476_540; // 2026-11-23T23:29:00Z
        let earlier = agy_pool_json(
            &[
                agy_account_json("acc1", "personal", false, 0, 0, "2026-11-23T23:29:00Z"),
                agy_account_json("acc2", "work", true, 98, 40, "2026-11-27T08:08:12Z"),
            ],
            2,
            Some(refill),
        );
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_agy_json(&mut snapshot, &earlier);
        let active = snapshot.get_card(ProviderId::Agy, Some("acc2")).expect("acc2 card");
        assert_eq!(active.pool_next_refill_epoch, Some(refill));
        assert_eq!(snapshot.get_card(ProviderId::Agy, Some("acc1")).expect("acc1").pool_next_refill_epoch, None);

        let later = agy_pool_json(
            &[agy_account_json("acc2", "work", true, 98, 40, "2026-11-20T08:08:12Z")],
            2,
            Some(refill),
        );
        update_snapshot_with_agy_json(&mut snapshot, &later);
        assert_eq!(agy_cards(&snapshot)[0].pool_next_refill_epoch, None);
    }

    #[test]
    fn iso_utc_timestamps_convert_to_epoch_seconds() {
        assert_eq!(iso_utc_to_epoch("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(iso_utc_to_epoch("2026-11-23T23:29:00Z"), Some(1_795_476_540));
        assert_eq!(iso_utc_to_epoch("2026-11-23T23:29:07.123Z"), Some(1_795_476_547));
        assert_eq!(iso_utc_to_epoch("2026-11-23T23:29:00"), None);
        assert_eq!(iso_utc_to_epoch("2026-13-01T00:00:00Z"), None);
        assert_eq!(iso_utc_to_epoch("garbage"), None);
    }

    fn tight_forecast(health: PacingHealth, runway_days: Option<f32>, five_hour_exhausted: bool) -> PacingForecast {
        let five_hour_forecast = five_hour_exhausted.then(|| FiveHourForecast {
            burn_rate: 1.0,
            pace_ratio: 1.5,
            runway_minutes: Some(10),
            delta_minutes: Some(-200),
            exhaustion_time: Some((14, 0)),
            reset_time: (16, 0),
            is_exhausted_before_reset: true,
        });
        PacingForecast {
            pace_ratio: Some(1.2),
            runway_days,
            delta_days: None,
            remaining_days: 6.0,
            health,
            badge_text: String::new(),
            summary_text: String::new(),
            five_hour_forecast,
            week_exhaustion_date: None,
            reset_str: None,
            today: (2026, 11, 20),
            week_state: WeekForecastState::Active,
            week_is_estimate: false,
            projected_week_reserve: None,
        }
    }

    #[test]
    fn pool_refill_caps_a_tight_warning_only_when_the_runway_reaches_the_refill() {
        let now = 1_795_000_000i64;
        let in_days = |days: f32| now + (days * 86_400.0) as i64;

        let mut covered = tight_forecast(PacingHealth::Tight, Some(6.0), false);
        apply_pool_refill_cap(&mut covered, Some(in_days(2.0)), now);
        assert_eq!(covered.health, PacingHealth::OnTrack);

        let mut not_covered = tight_forecast(PacingHealth::Tight, Some(1.0), false);
        apply_pool_refill_cap(&mut not_covered, Some(in_days(2.0)), now);
        assert_eq!(not_covered.health, PacingHealth::Tight);

        let mut refill_too_far = tight_forecast(PacingHealth::Tight, Some(6.0), false);
        apply_pool_refill_cap(&mut refill_too_far, Some(in_days(10.0)), now);
        assert_eq!(refill_too_far.health, PacingHealth::Tight);

        let mut five_hour_out = tight_forecast(PacingHealth::Tight, Some(6.0), true);
        apply_pool_refill_cap(&mut five_hour_out, Some(in_days(2.0)), now);
        assert_eq!(five_hour_out.health, PacingHealth::Tight);

        let mut depleted = tight_forecast(PacingHealth::Throttled, Some(0.0), false);
        apply_pool_refill_cap(&mut depleted, Some(in_days(2.0)), now);
        assert_eq!(depleted.health, PacingHealth::Throttled);

        let mut no_pool = tight_forecast(PacingHealth::Tight, Some(6.0), false);
        apply_pool_refill_cap(&mut no_pool, None, now);
        assert_eq!(no_pool.health, PacingHealth::Tight);
    }

    #[test]
    fn relogin_state_without_auth_error_key_blocks_the_account() {
        let json = agy_pool_json(
            &[r#"{"id":"acc2","label":"work","is_active":true,"auth_state":"relogin_required","five_hour_pct":90,"week_pct":90,"week_reset":"2026-11-27T08:08:12Z"}"#.to_string()],
            1,
            None,
        );
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_agy_json(&mut snapshot, &json);
        let card = snapshot.get_card(ProviderId::Agy, Some("acc2")).expect("acc2 card");
        assert_eq!(card.auth_error.as_deref(), Some("Login erneuern"));
    }

    #[test]
    fn relogin_state_keeps_an_explicit_auth_error_text() {
        let json = agy_pool_json(
            &[r#"{"id":"acc2","label":"work","is_active":true,"auth_state":"relogin_required","auth_error":"Re-login required","five_hour_pct":90,"week_pct":90,"week_reset":"2026-11-27T08:08:12Z"}"#.to_string()],
            1,
            None,
        );
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_agy_json(&mut snapshot, &json);
        let card = snapshot.get_card(ProviderId::Agy, Some("acc2")).expect("acc2 card");
        assert_eq!(card.auth_error.as_deref(), Some("Re-login required"));
    }

    #[test]
    fn ok_auth_state_adds_no_error() {
        let json = agy_pool_json(
            &[agy_account_json("acc2", "work", true, 90, 90, "2026-11-27T08:08:12Z")],
            1,
            None,
        );
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_agy_json(&mut snapshot, &json);
        assert_eq!(snapshot.get_card(ProviderId::Agy, Some("acc2")).expect("acc2 card").auth_error, None);
    }

    #[test]
    fn test_update_snapshot_with_agy_tsv() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let tsv = "Gemini Models\tFive Hour Limit Remaining\t92%\t2026-09-19T13:05:01Z\nGemini Models\tWeekly Limit Remaining\t89%\t2026-09-25T10:53:16Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-09-26T08:25:40Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-09-19T13:25:40Z";
        update_snapshot_with_agy_tsv(&mut snapshot, tsv);
        let agi = snapshot.get(ProviderId::Agy).expect("AGI exists");
        assert_eq!(agi.five_hour_percent, Some(92));
        assert_eq!(agi.week_percent, Some(89));
        // The reset conversion applies the local offset valid at test runtime
        // (Berlin yields 12:53 in summer time, 11:53 in winter time), so the
        // expectation mirrors exactly that rule instead of one season.
        let offset = local_timezone_offset_minutes();
        let total = (10i32 * 60 + 53 + offset).rem_euclid(24 * 60);
        let expected = format!("25.09. ({:02}:{:02})", total / 60, total % 60);
        assert_eq!(agi.week_reset, Some(expected));
        assert!(agi.five_hour_reset.is_some());
    }

    #[test]
    fn test_update_snapshot_with_glm_json() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let json = r#"{"code":200,"msg":"Operation successful","data":{"limits":[{"type":"TIME_LIMIT","unit":5,"number":1,"usage":4000,"currentValue":1039,"remaining":2961,"percentage":25,"nextResetTime":1790868874997,"usageDetails":[]},{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":7,"nextResetTime":1789822910132}],"level":"max"},"success":true}"#;
        update_snapshot_with_glm_json(&mut snapshot, json);
        let glm = snapshot.get(ProviderId::Glm).expect("GLM exists");
        assert_eq!(glm.five_hour_percent, Some(93)); // 100 - 7 = 93
        assert_eq!(glm.week_percent, Some(75)); // 100 - 25 = 75
        assert!(glm.week_reset.as_ref().unwrap().starts_with("01.10. ("));
        assert!(glm.week_reset.as_ref().unwrap().ends_with(')'));
        assert!(glm.five_hour_reset.is_some());
        assert_eq!(glm.cycle_label(), "Mo");
    }

    #[test]
    fn test_five_hour_pace_text_ratio() {
        let fc = pacing_forecast_for(
            false,
            Some(80),
            Some("25.09."),
            Some(96),
            Some("19:45"),
            Some((2026, 9, 19)),
            Some((15, 0)),
        );
        let pace_de = fc.five_hour_pace_text(Language::German);
        let pace_en = fc.five_hour_pace_text(Language::English);
        assert!(pace_de.starts_with("Pace: "), "Got: {pace_de}");
        assert!(pace_en.starts_with("Pace: "), "Got: {pace_en}");
        assert!(pace_de.contains("x · "), "Got: {pace_de}");
    }

    #[test]
    fn test_english_translations_for_reserve_and_pace() {
        // Scenario matching Simon's live status: 73% week, reset in 5 days, 52% 5h
        let fc = pacing_forecast_for(
            false,
            Some(73),
            Some("25.09."),
            Some(52),
            Some("21:14"),
            Some((2026, 9, 20)),
            Some((19, 15)),
        );

        let week_de = fc.week_pace_text(Language::German);
        let week_en = fc.week_pace_text(Language::English);
        assert!(week_de.contains("Reserve"), "DE got: {week_de}");
        assert!(week_en.contains("reserve"), "EN got: {week_en}");
        assert!(!week_de.contains("Ausgeglichen"), "DE got: {week_de}");
        assert!(!week_en.contains("on track"), "EN got: {week_en}");

        let fh_de = fc.five_hour_pace_text(Language::German);
        let fh_en = fc.five_hour_pace_text(Language::English);
        assert!(fh_de.contains("Reserve"), "DE got: {fh_de}");
        assert!(fh_en.contains("reserve"), "EN got: {fh_en}");

        let lower_en = fc.localized_lower_line(Language::English);
        assert!(lower_en.contains("reserve"), "EN got: {lower_en}");
    }

    #[test]
    fn test_five_hour_forecast_slow_usage_simon_scenario() {
        // Simon's exact scenario: 98% remaining (2% consumed), reset in 4h 51m (21:12), now 16:21
        // Elapsed = 10 mins, burn rate = 2.0 / 10 = 0.20%/min
        // Runway = 98.0 / 0.20 = 490 mins (~8.16 hours)
        // Must display concrete runway "Reicht noch ~8h", NEVER vague "Puffer stabil"!
        let today = (2026, 9, 20);
        let now = (16, 21);
        let fc = pacing_forecast_for(
            false,
            Some(76),
            Some("25.09."),
            Some(98),
            Some("21:12"),
            Some(today),
            Some(now),
        );
        let runway_de = fc.five_hour_runway_text(Language::German);
        let runway_en = fc.five_hour_runway_text(Language::English);
        assert_eq!(runway_de, "Reicht noch ~8h (00:31)");
        assert_eq!(runway_en, "Lasts ~8h (00:31)");
        assert!(!runway_de.contains("Puffer stabil"));
    }

    #[test]
    fn test_five_hour_forecast_zero_consumption() {
        // 100% remaining, 0% consumed -> runway >24h
        let today = (2026, 9, 20);
        let now = (14, 0);
        let fc = pacing_forecast_for(
            false,
            Some(100),
            Some("25.09."),
            Some(100),
            Some("19:00"),
            Some(today),
            Some(now),
        );
        assert_eq!(fc.five_hour_runway_text(Language::German), "Reicht >24h");
        assert_eq!(fc.five_hour_runway_text(Language::English), "Lasts >24h");
        assert_eq!(fc.week_runway_text(Language::German), "Reicht >30 Tage");
        assert_eq!(fc.week_runway_text(Language::English), "Lasts >30 days");
    }

    #[test]
    fn test_five_hour_empty_while_week_has_healthy_quota() {
        // Simon's scenario: 5h-limit exhausted (0%, reset in 9m at 16:55),
        // but week limit has 62% remaining (reset on 25.09. 12:53, today 21.09. 16:46).
        // Provider as a whole has is_throttled = true (because 5h is 0%).
        let today = (2026, 9, 21);
        let now = (16, 46);
        let fc = pacing_forecast_for(
            true, // is_throttled is true for the provider
            Some(62),
            Some("25.09. (12:53)"),
            Some(0),
            Some("16:55"),
            Some(today),
            Some(now),
        );
        // 5h must reflect exhaustion before reset with exact time
        assert_eq!(fc.five_hour_runway_text(Language::German), "Leer in: ~0m (16:46)");
        assert!(fc.five_hour_pace_text(Language::German).contains("Defizit"));

        // Week limit has 62% remaining and MUST NOT be throttled or show ~0h!
        let rw_days = fc.runway_days.expect("week runway must be calculated");
        assert!(rw_days >= 4.0, "runway days was {}, expected >= 4.0", rw_days);
        let week_text_de = fc.week_runway_text(Language::German);
        assert!(week_text_de.contains("Tage"), "week_text_de was '{}', expected 'Tage'", week_text_de);
        assert!(!week_text_de.contains("~0h"), "week limit must NEVER show ~0h when quota is 62%!");
        let week_pace_de = fc.week_pace_text(Language::German);
        assert!(!week_pace_de.is_empty(), "week pace text must not be empty");
        assert!(week_pace_de.contains("Reserve"), "week pace was '{}', expected Reserve", week_pace_de);
    }

    #[test]
    fn test_five_hour_exhaustion_time_display() {
        // Simon's scenario: 94% remaining, reset in 4h 47m (21:55), now 17:08.
        // Burns fast enough that exhaustion is predicted before reset.
        let today = (2026, 9, 21);
        let now = (17, 8);
        let fc = pacing_forecast_for(
            false,
            Some(61),
            Some("25.09. (12:53)"),
            Some(94),
            Some("21:55"),
            Some(today),
            Some(now),
        );
        let runway_de = fc.five_hour_runway_text(Language::German);
        assert!(runway_de.starts_with("Leer in: ~3h 24m"));
        assert!(runway_de.ends_with("(20:32)"));
    }

    #[test]
    fn test_week_exhaustion_date_display() {
        let today = (2026, 9, 22);
        let now = (10, 34);
        let fc = pacing_forecast_for(
            false,
            Some(55),
            Some("25.09. (12:53)"),
            Some(73),
            Some("13:37"),
            Some(today),
            Some(now),
        );
        let week_de = fc.week_runway_text(Language::German);
        assert_eq!(week_de, "Reicht noch 4.8 Tage (27.09)");
        let week_en = fc.week_runway_text(Language::English);
        assert_eq!(week_en, "Lasts 4.8 days (27.09)");
    }

    #[test]
    fn test_format_reset_time_phrase_simon_screenshot() {
        // Simon's scenario: 2026-09-23 at 12:36, reset on "23.09. (19:00)"
        let now_dt = (2026, 9, 23, 12, 36);
        let phrase_de = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::German, now_dt);
        let phrase_en = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::English, now_dt);
        assert_eq!(phrase_de, "in ~6h (heute 19:00)");
        assert_eq!(phrase_en, "in ~6h (today 19:00)");
    }

    #[test]
    fn missing_week_reset_does_not_invent_a_runway_or_pace() {
        let fc = pacing_forecast_for(
            false,
            Some(80),
            None,
            None,
            None,
            Some((2026, 9, 24)),
            Some((10, 17)),
        );

        assert_eq!(fc.pace_ratio, None);
        assert_eq!(fc.runway_days, None);
        assert_eq!(fc.week_runway_text(Language::German), "");
        assert_eq!(fc.week_pace_text(Language::German), "");
    }

    #[test]
    fn codex_weekly_forecast_uses_known_reset_for_simon_screenshot() {
        let fc = pacing_forecast_for(
            false,
            Some(80),
            Some("30.09. (19:44)"),
            None,
            None,
            Some((2026, 9, 24)),
            Some((10, 17)),
        );

        let elapsed = (24.0 - (19.0 + 44.0 / 60.0) + 10.0 + 17.0 / 60.0) / 24.0;
        assert!((fc.runway_days.unwrap() - 4.0 * elapsed).abs() < 0.0001);
        assert!((fc.pace_ratio.unwrap() - 1.4 / elapsed).abs() < 0.0001);
        assert_eq!(fc.week_runway_text(Language::German), "Reicht noch 2.4 Tage (26.09)");
        assert!(fc.week_pace_text(Language::German).contains("-4d Defizit"));
    }

    #[test]
    fn codex_october_screenshot_has_reserve_instead_of_three_day_deficit() {
        let fc = pacing_forecast_for(
            false, Some(95), Some("18.10. (07:35)"), None, None,
            Some((2026, 10, 11)), Some((18, 4)),
        );
        let elapsed = 629.0 / 1440.0; // 07:35 to 18:04 on cycle day one.
        assert!((fc.remaining_days - (7.0 - elapsed)).abs() < 0.0001);
        assert!((fc.pace_ratio.unwrap() - 0.35 / elapsed).abs() < 0.0001);
        assert!((fc.runway_days.unwrap() - 19.0 * elapsed).abs() < 0.0001);
        assert_eq!(fc.health, PacingHealth::Surplus);
        assert!(!fc.week_is_deficit());
        assert_eq!(fc.week_pace_text(Language::German), "Pace: 0.80x · +2d Reserve");
        assert_eq!(fc.week_runway_text(Language::German), "Reicht noch 8.3 Tage (20.10)");
    }

    #[test]
    fn weekly_pace_uses_actual_first_hour_instead_of_fixed_elapsed_floor() {
        let fc = pacing_forecast_for(
            false, Some(95), Some("18.10. (07:35)"), None, None,
            Some((2026, 10, 11)), Some((8, 35)),
        );
        assert!((fc.pace_ratio.unwrap() - 8.4).abs() < 0.001);
        assert!((fc.runway_days.unwrap() - 19.0 / 24.0).abs() < 0.0001);
        assert!(fc.week_is_deficit());
    }

    #[test]
    fn exact_weekly_reset_handles_year_boundary_and_same_day() {
        assert!((remaining_days_at(Some("02.01. (06:00)"), (2026, 12, 31), (18, 0))
            .unwrap() - 1.5).abs() < 0.0001);
        assert!((remaining_days_at(Some("11.10. (19:00)"), (2026, 10, 11), (18, 0))
            .unwrap() - 1.0 / 24.0).abs() < 0.0001);
    }

    #[test]
    fn test_format_reset_time_phrase_tomorrow_and_over_weekend() {
        let now_dt = (2026, 9, 23, 12, 36); // Wednesday noon

        // Tomorrow at 19:00 -> in ~1d 6h
        let phrase_tomorrow = format_reset_time_phrase(Some("24.09. (19:00)"), false, Language::German, now_dt);
        assert_eq!(phrase_tomorrow, "in ~1d 6h (24.09. 19:00)");

        // Friday at 12:53 -> in ~2d
        let phrase_fri = format_reset_time_phrase(Some("25.09. (12:53)"), false, Language::German, now_dt);
        assert_eq!(phrase_fri, "in ~2d (25.09. 12:53)");

        // Over the weekend to Monday 19:00 -> in ~5d 6h
        let phrase_mon = format_reset_time_phrase(Some("28.09. (19:00)"), false, Language::German, now_dt);
        assert_eq!(phrase_mon, "in ~5d 6h (28.09. 19:00)");
    }

    #[test]
    fn test_format_reset_time_phrase_shows_days_and_hours() {
        let now_dt = (2026, 9, 24, 10, 28);
        let phrase_de = format_reset_time_phrase(
            Some("30.09. (19:44)"),
            false,
            Language::German,
            now_dt,
        );
        let phrase_en = format_reset_time_phrase(
            Some("30.09. (19:44)"),
            false,
            Language::English,
            now_dt,
        );
        assert_eq!(phrase_de, "in ~6d 9h (30.09. 19:44)");
        assert_eq!(phrase_en, "in ~6d 9h (30.09. 19:44)");
    }

    #[test]
    fn test_format_reset_time_phrase_under_one_hour_and_due() {
        let now_dt = (2026, 9, 23, 18, 20); // 40 minutes before 19:00
        let phrase_de = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::German, now_dt);
        let phrase_en = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::English, now_dt);
        assert_eq!(phrase_de, "in 40m (heute 19:00)");
        assert_eq!(phrase_en, "in 40m (today 19:00)");

        // Passed reset time -> in Kürze / soon
        let now_past = (2026, 9, 23, 19, 5);
        let phrase_due_de = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::German, now_past);
        let phrase_due_en = format_reset_time_phrase(Some("23.09. (19:00)"), false, Language::English, now_past);
        assert_eq!(phrase_due_de, "in Kürze (19:00)");
        assert_eq!(phrase_due_en, "soon (19:00)");
    }

    #[test]
    fn test_format_reset_time_phrase_five_hour_window() {
        let now_dt = (2026, 9, 23, 12, 36);
        let phrase_5h = format_reset_time_phrase(Some("15:45"), true, Language::German, now_dt);
        assert_eq!(phrase_5h, "in 3h 9m (15:45)");
    }

    #[test]
    fn test_format_reset_time_phrase_date_only_and_month_rollover() {
        let now_dt = (2026, 9, 23, 12, 0);
        let phrase_date_only_de = format_reset_time_phrase(Some("25.09."), false, Language::German, now_dt);
        let phrase_date_only_en = format_reset_time_phrase(Some("25.09."), false, Language::English, now_dt);
        assert_eq!(phrase_date_only_de, "in ~48h (25.09.)");
        assert_eq!(phrase_date_only_en, "in ~48h (25.09.)");

        // Month boundary: Sept 30 20:00 to Oct 01 14:00 (18 hours)
        let now_sept30 = (2026, 9, 30, 20, 0);
        let phrase_oct_de = format_reset_time_phrase(Some("01.10. (14:00)"), false, Language::German, now_sept30);
        let phrase_oct_en = format_reset_time_phrase(Some("01.10. (14:00)"), false, Language::English, now_sept30);
        assert_eq!(phrase_oct_de, "in ~18h (01.10. 14:00)");
        assert_eq!(phrase_oct_en, "in ~18h (01.10. 14:00)");
    }

    #[test]
    fn test_format_reset_time_phrase_english_translations() {
        let now_dt = (2026, 9, 23, 12, 36);

        // No schedule
        assert_eq!(format_reset_time_phrase(None, false, Language::German, now_dt), "nicht terminiert");
        assert_eq!(format_reset_time_phrase(None, false, Language::English, now_dt), "no schedule");

        // 5h > 300m
        assert_eq!(format_reset_time_phrase(Some("18:00"), true, Language::German, (2026, 9, 23, 12, 0)), "um 18:00");
        assert_eq!(format_reset_time_phrase(Some("18:00"), true, Language::English, (2026, 9, 23, 12, 0)), "at 18:00");

        // Far future (>240h, e.g. monthly quota 20 days)
        let phrase_monthly_de = format_reset_time_phrase(Some("13.10. (12:00)"), false, Language::German, now_dt);
        let phrase_monthly_en = format_reset_time_phrase(Some("13.10. (12:00)"), false, Language::English, now_dt);
        assert_eq!(phrase_monthly_de, "in ~20d (13.10. 12:00)");
        assert_eq!(phrase_monthly_en, "in ~20d (13.10. 12:00)");

        // Unparseable raw fallback
        assert_eq!(format_reset_time_phrase(Some("unbekannt"), false, Language::German, now_dt), "in unbekannt");
        assert_eq!(format_reset_time_phrase(Some("unknown"), false, Language::English, now_dt), "in unknown");
    }

    fn codex_from_json(json_str: &str) -> ProviderQuota {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        update_snapshot_with_codex_json(&mut snapshot, json_str);
        snapshot.get(ProviderId::Codex).unwrap().clone()
    }

    #[test]
    fn codex_expired_session_data_does_not_clear_an_unassigned_account_throttle() {
        let reset = 1_800_604_800;
        let codex = codex_from_json(&format!(
            r#"{{"is_throttled":true,"five_hour":{{"remaining_percent":0,"consumed_percent":100,"reset_epoch":{reset},"source":"app_server"}}}}"#
        ));
        assert_eq!(codex.five_hour_window_at(reset - 1).0, Some(0));
        assert!(codex.codex_pacing_forecast_at(reset - 1, (2027, 1, 20), (11, 59)).five_hour_forecast.is_some());
        assert_eq!(codex.five_hour_window_at(reset), (None, None));
        assert!(codex.codex_pacing_forecast_at(reset, (2027, 1, 20), (12, 0)).five_hour_forecast.is_none());
        assert!(codex.provider_is_throttled_at(reset));
    }

    #[test]
    fn codex_deficit_keeps_its_size_when_the_projected_runway_has_elapsed() {
        let reset = 1_800_604_800;
        let observed = reset - 7_200;
        let codex = codex_from_json(&format!(
            r#"{{"week":{{"remaining_percent":0,"consumed_percent":99.9,"reset_epoch":{reset},"observed_epoch":{observed},"source":"session_log"}}}}"#
        ));
        let before = codex.codex_pacing_forecast_at(observed, (2027, 1, 20), (10, 0));
        let after = codex.codex_pacing_forecast_at(reset - 1_800, (2027, 1, 20), (11, 30));
        assert_eq!(after.runway_days, Some(0.0));
        assert_eq!(before.week_pace_text(Language::German), after.week_pace_text(Language::German));
        assert!(after.week_pace_text(Language::German).contains("~-2h Defizit"));
    }

    #[test]
    fn codex_a1_21_minutes_1_percent_stays_pending_with_reason() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_epoch = window_start + 21 * 60;
        let json = format!(
            r#"{{"week":{{"remaining_percent":99,"consumed_percent":1.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_epoch},"source":"app_server"}}}}"#
        );
        let codex = codex_from_json(&json);
        let fc = codex.codex_pacing_forecast_at(obs_epoch, (2027, 1, 20), (12, 21));
        assert_eq!(fc.week_state, WeekForecastState::Pending(WeekPendingReason::EarlyWindowAge));
        assert_eq!(fc.pace_ratio, None);
        assert_eq!(fc.runway_days, None);
        assert_eq!(fc.delta_days, None);
        assert_eq!(fc.week_runway_text(Language::German), "Schätzung ausstehend");
        assert_eq!(fc.week_runway_text(Language::English), "Estimate pending");
        assert_eq!(fc.week_pace_text(Language::German), "Datenbasis zu kurz");
        assert_eq!(fc.week_pace_text(Language::English), "Window too young");
    }

    #[test]
    fn codex_a2_app_server_minute_59_pending_minute_60_nominal_3_36x_neutral() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_59 = window_start + 59 * 60;
        let json_59 = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_59},"source":"app_server"}}}}"#
        );
        let fc_59 = codex_from_json(&json_59).codex_pacing_forecast_at(obs_59, (2027, 1, 20), (12, 59));
        assert_eq!(fc_59.week_state, WeekForecastState::Pending(WeekPendingReason::EarlyWindowAge));
        assert_eq!(fc_59.pace_ratio, None);

        let obs_60 = window_start + 60 * 60;
        let json_60 = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_60},"source":"app_server"}}}}"#
        );
        let codex_60 = codex_from_json(&json_60);
        let fc_60 = codex_60.codex_pacing_forecast_at(obs_60, (2027, 1, 20), (13, 0));
        assert_eq!(fc_60.week_state, WeekForecastState::EstimateNeutral);
        assert!(fc_60.week_is_estimate);
        let ratio = fc_60.pace_ratio.expect("pace ratio present at minute 60");
        assert!((ratio - 3.36).abs() < 0.01, "expected 3.36x, got {ratio}");
        assert!(fc_60.week_pace_text(Language::German).contains("Pace: ~3.36x"));
        assert!(fc_60.week_runway_text(Language::German).contains("~"));
        let light = runway_light(false, 98, false, &fc_60);
        assert_eq!(light.color, RunwayColor::Gray);
        assert!(!light.red && !light.yellow);
        assert_eq!(week_pace_light(false, 98, &light, &fc_60), RunwayColor::Gray);
    }

    #[test]
    fn codex_a3_proven_span_crossing_reset_is_neutral_estimate() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_4h = window_start + 4 * 3_600;
        let json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"consumed_range":[1.5,2.5],"reset_epoch":{reset_epoch},"observed_epoch":{obs_4h},"source":"session_log"}}}}"#
        );
        let fc = codex_from_json(&json).codex_pacing_forecast_at(obs_4h, (2027, 1, 20), (16, 0));
        assert_eq!(fc.week_state, WeekForecastState::EstimateNeutral);
        assert!(fc.week_is_estimate);
        let light = runway_light(false, 98, false, &fc);
        assert_eq!(light.color, RunwayColor::Gray);
        assert_eq!(week_pace_light(false, 98, &light, &fc), RunwayColor::Gray);
        assert!(fc.week_pace_text(Language::German).contains("~"));
        assert!(fc.week_runway_text(Language::English).contains("~"));
    }

    #[test]
    fn codex_a4_proven_span_at_1h_deficit_6h_reserve_and_equality_neutral() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;

        // 1 hour: [1.5%, 2.5%] -> pace [2.52x, 4.20x] -> both end before reset -> Deficit
        let obs_1h = window_start + 3_600;
        let json_1h = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"consumed_range":[1.5,2.5],"reset_epoch":{reset_epoch},"observed_epoch":{obs_1h},"source":"session_log"}}}}"#
        );
        let fc_1h = codex_from_json(&json_1h).codex_pacing_forecast_at(obs_1h, (2027, 1, 20), (13, 0));
        assert_eq!(fc_1h.week_state, WeekForecastState::ProvenDeficit);
        assert!(!fc_1h.week_is_estimate);
        let light_1h = runway_light(false, 98, false, &fc_1h);
        assert_eq!(light_1h.color, RunwayColor::Red);
        assert!(fc_1h.week_pace_text(Language::German).contains("Defizit"));

        // 6 hours: [1.5%, 2.5%] -> pace [0.42x, 0.70x] -> both reach beyond reset -> Reserve
        let obs_6h = window_start + 6 * 3_600;
        let json_6h = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"consumed_range":[1.5,2.5],"reset_epoch":{reset_epoch},"observed_epoch":{obs_6h},"source":"session_log"}}}}"#
        );
        let fc_6h = codex_from_json(&json_6h).codex_pacing_forecast_at(obs_6h, (2027, 1, 20), (18, 0));
        assert_eq!(fc_6h.week_state, WeekForecastState::Active);
        assert!(!fc_6h.week_is_estimate);
        let light_6h = runway_light(false, 98, false, &fc_6h);
        assert_eq!(light_6h.color, RunwayColor::Green);
        assert!(fc_6h.week_pace_text(Language::German).contains("Reserve"));

        // Equality on one endpoint (2.5% at 4.2h => 1.00x exactly) stays neutral
        let obs_eq = window_start + (42 * 360); // 4.2 hours = 15120s
        let json_eq = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"consumed_range":[1.5,2.5],"reset_epoch":{reset_epoch},"observed_epoch":{obs_eq},"source":"session_log"}}}}"#
        );
        let fc_eq = codex_from_json(&json_eq).codex_pacing_forecast_at(obs_eq, (2027, 1, 20), (16, 12));
        assert_eq!(fc_eq.week_state, WeekForecastState::EstimateNeutral);
    }

    #[test]
    fn codex_a5_12h_98_percent_shows_18d_reserve_and_30_day_boundary() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_12h = window_start + 12 * 3_600;
        let json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_12h},"source":"app_server"}}}}"#
        );
        let fc = codex_from_json(&json).codex_pacing_forecast_at(obs_12h, (2027, 1, 20), (24, 0));
        assert!((fc.runway_days.unwrap() - 24.5).abs() < 0.05);
        assert_eq!(fc.week_runway_text(Language::German), "Reicht ~24.5 Tage (14.02)");
        assert_eq!(fc.week_runway_text(Language::English), "Lasts ~24.5 days (14.02)");
        assert_eq!(fc.week_pace_text(Language::German), "Pace: ~0.28x · ~+18d Reserve");
        assert_eq!(fc.week_pace_text(Language::English), "Pace: ~0.28x · ~+18d reserve");

        // Boundary: exactly 30.0 days reserve stays 30d; >30.0 days uses >30 Tage/days
        let mut fc_30 = fc.clone();
        fc_30.week_state = WeekForecastState::Active;
        fc_30.week_is_estimate = false;
        fc_30.remaining_days = 5.0;
        fc_30.runway_days = Some(35.0);
        fc_30.projected_week_reserve = Some(30.0);
        assert_eq!(fc_30.week_pace_text(Language::German), "Pace: 0.28x · +30d Reserve");
        assert_eq!(fc_30.week_pace_text(Language::English), "Pace: 0.28x · +30d reserve");

        fc_30.runway_days = Some(35.01);
        fc_30.projected_week_reserve = Some(30.01);
        assert_eq!(fc_30.week_pace_text(Language::German), "Pace: 0.28x · +>30 Tage Reserve");
        assert_eq!(fc_30.week_pace_text(Language::English), "Pace: 0.28x · +>30 days reserve");
    }

    #[test]
    fn codex_a6_throttle_attribution_rounded_zero_log_and_reset_expiry() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs = window_start + 2 * 86_400;

        // 1. Provider-level throttle with two windows and no weekly 0% -> Codex-level throttled, week NOT confirmed exhausted
        let unattributed = format!(
            r#"{{"is_throttled":true,"five_hour":{{"remaining_percent":60,"reset_epoch":{reset_epoch}}},"week":{{"remaining_percent":75,"consumed_percent":25.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"session_log"}}}}"#
        );
        let codex_unattr = codex_from_json(&unattributed);
        assert!(codex_unattr.is_throttled);
        assert!(!codex_unattr.week_is_throttled());
        let fc_unattr = codex_unattr.codex_pacing_forecast_at(obs, (2027, 1, 20), (12, 0));
        assert_ne!(fc_unattr.week_state, WeekForecastState::ConfirmedExhausted);

        // 2. Confirmed weekly exhaustion (`week_throttled: true`) -> both Codex-level and week-level throttled
        let confirmed = format!(
            r#"{{"is_throttled":true,"week_throttled":true,"week":{{"remaining_percent":0,"consumed_percent":100.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server"}}}}"#
        );
        let codex_conf = codex_from_json(&confirmed);
        assert!(codex_conf.provider_is_throttled_at(obs));
        assert!(codex_conf.week_confirmed_exhausted);
        let fc_conf = codex_conf.codex_pacing_forecast_at(obs, (2027, 1, 20), (12, 0));
        assert_eq!(fc_conf.week_state, WeekForecastState::ConfirmedExhausted);
        assert_eq!(fc_conf.week_runway_text(Language::German), "Erschöpft");

        // 3. Rounded zero in session_log (99.6% consumed) -> lock NOT confirmed
        let rounded_zero = format!(
            r#"{{"week":{{"remaining_percent":0,"consumed_percent":99.6,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"session_log"}}}}"#
        );
        let codex_rz = codex_from_json(&rounded_zero);
        assert!(!codex_rz.is_throttled);
        assert!(!codex_rz.week_confirmed_exhausted);

        // 4. Subsequent reset without new fetch -> old forecast AND provider throttle expire
        assert!(!codex_conf.provider_is_throttled_at(reset_epoch + 1));
        let fc_expired = codex_conf.codex_pacing_forecast_at(reset_epoch + 1, (2027, 1, 27), (12, 0));
        assert_eq!(fc_expired.week_state, WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing));
        assert_ne!(fc_expired.health, PacingHealth::Throttled);
        assert_eq!(fc_expired.pace_ratio, None);
        assert_eq!(fc_expired.runway_days, None);
    }

    #[test]
    fn codex_a7_missing_zero_or_invalid_window_has_no_fabricated_forecast() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_2d = window_start + 2 * 86_400;

        // Zero consumption -> Runway open, no numbers
        let zero_json = format!(
            r#"{{"week":{{"remaining_percent":100,"consumed_percent":0.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_2d},"source":"app_server"}}}}"#
        );
        let fc_zero = codex_from_json(&zero_json).codex_pacing_forecast_at(obs_2d, (2027, 1, 20), (12, 0));
        assert_eq!(fc_zero.week_state, WeekForecastState::Pending(WeekPendingReason::ZeroUsage));
        assert_eq!(fc_zero.pace_ratio, None);
        assert_eq!(fc_zero.runway_days, None);
        assert_eq!(fc_zero.week_runway_text(Language::German), "Reichweite offen");

        // Invalid window (observed before window start)
        let invalid_json = format!(
            r#"{{"week":{{"remaining_percent":90,"consumed_percent":10.0,"reset_epoch":{reset_epoch},"observed_epoch":{},"source":"session_log"}}}}"#,
            window_start - 60
        );
        let fc_invalid = codex_from_json(&invalid_json).codex_pacing_forecast_at(obs_2d, (2027, 1, 20), (12, 0));
        assert_eq!(fc_invalid.week_state, WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing));
        assert_eq!(fc_invalid.pace_ratio, None);
        assert_eq!(fc_invalid.runway_days, None);

        // Missing telemetry
        let fc_missing = codex_from_json("null").codex_pacing_forecast_at(obs_2d, (2027, 1, 20), (12, 0));
        assert_eq!(fc_missing.week_state, WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing));
        assert_eq!(fc_missing.pace_ratio, None);
    }

    #[test]
    fn codex_a8_10_percent_after_30_minutes_shows_33_6x() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_30m = window_start + 30 * 60;

        // AppServer: nominal 33.60x estimate, neutral light
        let app_json = format!(
            r#"{{"week":{{"remaining_percent":90,"consumed_percent":10.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_30m},"source":"app_server"}}}}"#
        );
        let fc_app = codex_from_json(&app_json).codex_pacing_forecast_at(obs_30m, (2027, 1, 20), (12, 30));
        assert!((fc_app.pace_ratio.unwrap() - 33.6).abs() < 0.05);
        assert_eq!(fc_app.week_state, WeekForecastState::EstimateNeutral);
        assert_eq!(runway_light(false, 90, false, &fc_app).color, RunwayColor::Gray);

        // Single SessionLog without proven interval -> nominal 33.60x estimate, neutral light
        let log_json = format!(
            r#"{{"week":{{"remaining_percent":90,"consumed_percent":10.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_30m},"source":"session_log"}}}}"#
        );
        let fc_log = codex_from_json(&log_json).codex_pacing_forecast_at(obs_30m, (2027, 1, 20), (12, 30));
        assert!((fc_log.pace_ratio.unwrap() - 33.6).abs() < 0.05);
        assert_eq!(fc_log.week_state, WeekForecastState::EstimateNeutral);
        assert_eq!(runway_light(false, 90, false, &fc_log).color, RunwayColor::Gray);

        // Proven interval [9.5, 10.5] -> proven deficit, Red light
        let proven_json = format!(
            r#"{{"week":{{"remaining_percent":90,"consumed_percent":10.0,"consumed_range":[9.5,10.5],"reset_epoch":{reset_epoch},"observed_epoch":{obs_30m},"source":"session_log"}}}}"#
        );
        let fc_proven = codex_from_json(&proven_json).codex_pacing_forecast_at(obs_30m, (2027, 1, 20), (12, 30));
        assert!((fc_proven.pace_ratio.unwrap() - 33.6).abs() < 0.05);
        assert_eq!(fc_proven.week_state, WeekForecastState::ProvenDeficit);
        assert_eq!(runway_light(false, 90, false, &fc_proven).color, RunwayColor::Red);
    }

    #[test]
    fn codex_a9_observation_time_controls_threshold_and_12h_1_percent_enters() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_59m = window_start + 59 * 60;
        let now_65m = window_start + 65 * 60;

        let old_json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_59m},"source":"session_log"}}}}"#
        );
        let fc_redraw = codex_from_json(&old_json).codex_pacing_forecast_at(now_65m, (2027, 1, 20), (13, 5));
        assert_eq!(fc_redraw.week_state, WeekForecastState::Pending(WeekPendingReason::EarlyWindowAge));
        assert_eq!(fc_redraw.pace_ratio, None);

        // New observation at 12h with 1% consumed -> enters forecast!
        let obs_12h = window_start + 12 * 3_600;
        let new_json = format!(
            r#"{{"week":{{"remaining_percent":99,"consumed_percent":1.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_12h},"source":"app_server"}}}}"#
        );
        let fc_12h = codex_from_json(&new_json).codex_pacing_forecast_at(obs_12h, (2027, 1, 20), (24, 0));
        assert_eq!(fc_12h.week_state, WeekForecastState::EstimateNeutral);
        assert!(fc_12h.pace_ratio.is_some());
        assert!(fc_12h.runway_days.is_some());
    }

    #[test]
    fn codex_regression_app_server_fetch_shape_and_100_percent_signal() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs = window_start + 2 * 86_400;

        // App-server integer 100% with envelope throttle (`is_throttled: true`,
        // `rate_limit_reached_type: "rate_limit_reached"`) -> provider lock, NOT confirmed weekly exhaustion
        let app_100_unattr = format!(
            r#"{{"is_throttled":true,"rate_limit_reached_type":"rate_limit_reached","five_hour":{{"remaining_percent":50,"consumed_percent":50.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":300}},"week":{{"remaining_percent":0,"consumed_percent":100.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":10080}}}}"#
        );
        let codex_unattr = codex_from_json(&app_100_unattr);
        assert!(codex_unattr.provider_is_throttled_at(obs));
        assert!(!codex_unattr.week_confirmed_exhausted);
        let fc_unattr = codex_unattr.codex_pacing_forecast_at(obs, (2027, 1, 20), (12, 0));
        assert_ne!(fc_unattr.week_state, WeekForecastState::ConfirmedExhausted);

        // App-server ordinaryUsageAllowed=false without rateLimitReachedType on two windows
        let app_no_reason = format!(
            r#"{{"is_throttled":true,"five_hour":{{"remaining_percent":20,"consumed_percent":80.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":300}},"week":{{"remaining_percent":80,"consumed_percent":20.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":10080}}}}"#
        );
        let codex_no_reason = codex_from_json(&app_no_reason);
        assert!(codex_no_reason.provider_is_throttled_at(obs));
        assert!(!codex_no_reason.week_confirmed_exhausted);
        let fc_no_reason = codex_no_reason.codex_pacing_forecast_at(obs, (2027, 1, 20), (12, 0));
        assert_eq!(fc_no_reason.week_state, WeekForecastState::EstimateNeutral);
        assert!(fc_no_reason.pace_ratio.is_some());

        // App-server 5h lock (`five_hour_throttled: true`) -> provider throttled, week NOT throttled
        let app_5h_lock = format!(
            r#"{{"is_throttled":true,"five_hour_throttled":true,"rate_limit_reached_type":"five_hour","five_hour":{{"remaining_percent":0,"consumed_percent":100.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":300}},"week":{{"remaining_percent":80,"consumed_percent":20.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server","window_minutes":10080}}}}"#
        );
        let codex_5h = codex_from_json(&app_5h_lock);
        assert!(codex_5h.provider_is_throttled_at(obs));
        assert!(codex_5h.five_hour_confirmed_exhausted);
        assert!(!codex_5h.week_confirmed_exhausted);
    }

    #[test]
    fn codex_regression_session_log_without_interval_stays_neutral_estimate() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs = window_start + 6 * 3_600;
        let log_json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"session_log","window_minutes":10080}}}}"#
        );
        let fc = codex_from_json(&log_json).codex_pacing_forecast_at(obs, (2027, 1, 20), (18, 0));
        assert_eq!(fc.week_state, WeekForecastState::EstimateNeutral);
        assert!(fc.week_is_estimate);
        assert_eq!(runway_light(false, 98, false, &fc).color, RunwayColor::Gray);
    }

    #[test]
    fn codex_regression_sample_age_subtracts_from_runway_and_future_rejected() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_12h = window_start + 12 * 3_600;
        let now_18h = window_start + 18 * 3_600; // sample age = 6h = 0.25d

        let json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs_12h},"source":"app_server"}}}}"#
        );
        let codex = codex_from_json(&json);

        // At now_18h: rate still comes from obs_12h (2% in 0.5d = 4%/d),
        // runway_from_obs = 98/4 = 24.5d, sample_age = 0.25d -> runway_from_now = 24.25d,
        // remaining_from_now = 6.25d -> reserve = 24.25 - 6.25 = 18.0d!
        let fc = codex.codex_pacing_forecast_at(now_18h, (2027, 1, 20), (18, 0));
        assert!((fc.runway_days.unwrap() - 24.25).abs() < 0.02);
        assert!((fc.remaining_days - 6.25).abs() < 0.02);
        assert_eq!(fc.delta_days, Some(18));
        assert_eq!(fc.week_pace_text(Language::German), "Pace: ~0.28x · ~+18d Reserve");

        // Future observation (`observed_epoch > now_epoch`) must be rejected
        let fc_future = codex.codex_pacing_forecast_at(obs_12h - 60, (2027, 1, 20), (11, 59));
        assert_eq!(fc_future.week_state, WeekForecastState::Pending(WeekPendingReason::InvalidOrMissing));
        assert_eq!(fc_future.pace_ratio, None);
        assert_eq!(fc_future.runway_days, None);
    }

    #[test]
    fn codex_judge_1_estimate_neutral_at_95_percent_used_keeps_neutral_text_and_warns_quota() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs = window_start + 2 * 86_400;
        let json = format!(
            r#"{{"week":{{"remaining_percent":5,"consumed_percent":95.0,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"app_server"}}}}"#
        );
        let codex = codex_from_json(&json);
        let fc = codex.codex_pacing_forecast_at(obs, (2027, 1, 22), (12, 0));
        assert_eq!(fc.week_state, WeekForecastState::EstimateNeutral);

        let light = runway_light(false, 5, false, &fc);
        assert_eq!(light.color, RunwayColor::Gray);
        assert!(light.red);
        assert_eq!(week_pace_light(false, 5, &light, &fc), RunwayColor::Gray);
    }

    #[test]
    fn codex_judge_2_proven_deficit_38m_at_4h_formats_as_deficit_in_de_and_en() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs_4h = window_start + 4 * 3_600;
        let json = format!(
            r#"{{"week":{{"remaining_percent":98,"consumed_percent":2.39,"consumed_range":[2.385,2.395],"reset_epoch":{reset_epoch},"observed_epoch":{obs_4h},"source":"session_log"}}}}"#
        );
        let fc = codex_from_json(&json).codex_pacing_forecast_at(obs_4h, (2027, 1, 20), (16, 0));
        assert_eq!(fc.week_state, WeekForecastState::ProvenDeficit);

        let light = runway_light(false, 98, false, &fc);
        assert_eq!(light.color, RunwayColor::Yellow);
        assert_eq!(week_pace_light(false, 98, &light, &fc), RunwayColor::Yellow);

        let pace_de = fc.week_pace_text(Language::German);
        let pace_en = fc.week_pace_text(Language::English);
        assert!(pace_de.contains("-38m Defizit"), "got {pace_de}");
        assert!(pace_en.contains("-38m deficit"), "got {pace_en}");
        assert!(!pace_de.contains("±0h"));
        assert!(!pace_en.contains("±0h"));
    }

    #[test]
    fn codex_judge_3_explicit_throttle_without_windows_sets_led_throttled() {
        let codex = codex_from_json(r#"{"is_throttled":true}"#);
        assert!(codex.provider_is_throttled());
        let fc = codex.codex_pacing_forecast_at(1_800_100_000, (2027, 1, 20), (12, 0));
        assert_eq!(fc.health, PacingHealth::Throttled);
        assert_ne!(fc.week_state, WeekForecastState::ConfirmedExhausted);
    }

    #[test]
    fn codex_judge_r2_rounded_zero_99_6_keeps_both_forecast_texts_neutral() {
        let reset_epoch: i64 = 1_800_604_800;
        let window_start = reset_epoch - 7 * 86_400;
        let obs = window_start + 2 * 86_400;
        let json = format!(
            r#"{{"week":{{"remaining_percent":0,"consumed_percent":99.6,"reset_epoch":{reset_epoch},"observed_epoch":{obs},"source":"session_log"}}}}"#
        );
        let codex = codex_from_json(&json);
        assert!(!codex.provider_is_throttled_at(obs));
        assert!(!codex.week_confirmed_exhausted);

        let fc = codex.codex_pacing_forecast_at(obs, (2027, 1, 22), (12, 0));
        assert_eq!(fc.week_state, WeekForecastState::EstimateNeutral);

        let light = runway_light(false, 0, false, &fc);
        assert_eq!(light.color, RunwayColor::Gray);
        assert!(light.red);
        assert_eq!(week_pace_light(false, 0, &light, &fc), RunwayColor::Gray);
    }
}
