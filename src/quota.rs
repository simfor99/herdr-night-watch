//! KI-Budget-Limits (GLM, Agy, Codex) Telemetry & Management for Herdr Night Watch.
//!
//! Provides data structures, parsers, and background sampling for provider quotas.
//! Fail-closed: values are strictly informational and never participate in shutdown decisions.

use crate::language::Language;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    Glm,
    Agy,
    Codex,
    Claude,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderQuota {
    pub id: ProviderId,
    pub title: &'static str,
    pub author: &'static str,
    pub five_hour_percent: Option<u8>,
    pub five_hour_reset: Option<String>,
    pub week_percent: Option<u8>,
    pub week_reset: Option<String>,
    pub is_throttled: bool,
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
        let is_throttled = five_hour_percent.map(|p| p == 0).unwrap_or(false)
            || week_percent.map(|p| p == 0).unwrap_or(false);
        Self {
            id,
            title,
            author,
            five_hour_percent,
            five_hour_reset,
            week_percent,
            week_reset,
            is_throttled,
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
        if self.id == ProviderId::Glm {
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

    pub fn pacing_forecast(&self) -> PacingForecast {
        pacing_forecast_for(
            self.is_throttled,
            self.week_percent,
            self.week_reset.as_deref(),
            self.five_hour_percent,
            self.five_hour_reset.as_deref(),
            None,
            None,
        )
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
        if let Some(days) = self.runway_days {
            let date_suffix = if let Some((m, d)) = self.week_exhaustion_date {
                format!(" ({d:02}.{m:02})")
            } else {
                String::new()
            };
            if days >= 30.0 {
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
            language.text("Reicht >30 Tage", "Lasts >30 days").to_string()
        }
    }

    pub fn week_pace_text(&self, language: Language) -> String {
        if let Some(ratio) = self.pace_ratio {
            let qualifier = if let Some(runway) = self.runway_days {
                let diff = runway - self.remaining_days;
                if diff < -0.04 {
                    if diff <= -1.5 {
                        let d = (-diff).round() as i32;
                        format!("-{}d {}", d, language.text("Defizit", "deficit"))
                    } else if diff <= -0.85 {
                        format!("-1d {}", language.text("Defizit", "deficit"))
                    } else {
                        let h = ((-diff) * 24.0).round() as i32;
                        if h >= 20 {
                            format!("-1d {}", language.text("Defizit", "deficit"))
                        } else {
                            format!("-{}h {}", h.max(1), language.text("Defizit", "deficit"))
                        }
                    }
                } else if diff > 0.04 {
                    if diff > 14.0 {
                        language.text("+>30 Tage Reserve", "+>30 days reserve").to_string()
                    } else if diff > 7.0 {
                        language.text("+>7 Tage Reserve", "+>7 days reserve").to_string()
                    } else if diff >= 1.5 {
                        let d = diff.round() as i32;
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
                } else if delta > 14 {
                    language.text("+>30 Tage Reserve", "+>30 days reserve").to_string()
                } else if delta > 7 {
                    language.text("+>7 Tage Reserve", "+>7 days reserve").to_string()
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
            format!("Pace: {:.2}x · {}", ratio, qualifier)
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

pub fn pacing_forecast_for(
    is_throttled: bool,
    week_percent: Option<u8>,
    week_reset: Option<&str>,
    five_hour_percent: Option<u8>,
    five_hour_reset: Option<&str>,
    custom_today: Option<(i32, u32, u32)>,
    custom_time: Option<(u32, u32)>,
) -> PacingForecast {
    let today = custom_today.unwrap_or_else(current_ymd);
    let now_hm = custom_time.unwrap_or_else(current_hm);

    let five_hour_forecast = calculate_five_hour_forecast(five_hour_percent, five_hour_reset, now_hm);

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
        let rem_days = parse_remaining_days(week_reset, today).unwrap_or(5.0);
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
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    let remaining_days = parse_remaining_days(week_reset, today).unwrap_or(7.0).max(0.1);
    let total_days = if remaining_days > 7.5 { 30.0 } else { 7.0 };
    let elapsed_days = (total_days - remaining_days).max(0.2);
    let consumed_pct = (100u8.saturating_sub(rem_pct)) as f32;

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
                let add_days = rw.round() as i64;
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
        };
        fc.badge_text = fc.localized_upper_line(Language::German);
        fc.summary_text = fc.localized_lower_line(Language::German);
        return fc;
    }

    let runway_days = (rem_pct as f32) / burn_rate;
    let today_days = ymd_to_days(today.0, today.1, today.2);
    let add_days = runway_days.round() as i64;
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
    };
    fc.badge_text = fc.localized_upper_line(Language::German);
    fc.summary_text = fc.localized_lower_line(Language::German);
    fc
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
                "AGI",
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
    snapshot.has_throttle = snapshot.providers.iter().any(|p| p.is_throttled);
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
    snapshot.has_throttle = snapshot.providers.iter().any(|p| p.is_throttled);
    snapshot.last_updated = Some(Instant::now());
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
    for (key, percent, reset) in [
        ("week", &mut codex.week_percent, &mut codex.week_reset),
        ("five_hour", &mut codex.five_hour_percent, &mut codex.five_hour_reset),
    ] {
        if let Some(window) = value.as_ref().and_then(|v| v.get(key)) {
            *percent = window.get("remaining_percent").and_then(|p| p.as_u64()).filter(|p| *p <= 100).map(|p| p as u8);
            *reset = window.get("reset").and_then(|r| r.as_str()).map(str::to_owned);
        }
    }
    codex.is_throttled = codex.week_percent == Some(0) || codex.five_hour_percent == Some(0);
    snapshot.has_throttle = snapshot.providers.iter().any(|p| p.is_throttled);
    snapshot.last_updated = Some(Instant::now());
}

#[cfg(windows)]
pub fn fetch_live_snapshot(current: &QuotaSnapshot) -> QuotaSnapshot {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut updated = current.clone();

    let distro = crate::configuration::load().distro;

    // 1. Fetch AGY
    let mut agy_cmd = Command::new("wsl.exe");
    agy_cmd.creation_flags(CREATE_NO_WINDOW)
        .arg("-d")
        .arg(&distro)
        .arg("--exec")
        .arg("/home/simon/.local/bin/agy")
        .arg("-p")
        .arg("/usage");

    if let Ok(output) = agy_cmd.output() {
        if output.status.success() {
            let tsv = String::from_utf8_lossy(&output.stdout);
            update_snapshot_with_agy_tsv(&mut updated, &tsv);
        }
    }

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
        .arg("/home/simon/projects/herdr-night-watch/tools/fetch_codex_quota.py");
    let codex_json = codex_cmd.output().ok().filter(|output| output.status.success());
    update_snapshot_with_codex_json(
        &mut updated,
        codex_json.as_ref().map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok()).unwrap_or("null"),
    );

    updated
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn fetch_live_snapshot(current: &QuotaSnapshot) -> QuotaSnapshot {
    let mut updated = current.clone();
    if let Ok(output) = std::process::Command::new("/home/simon/.local/bin/agy")
        .args(["-p", "/usage"])
        .output()
    {
        if output.status.success() {
            let tsv = String::from_utf8_lossy(&output.stdout);
            update_snapshot_with_agy_tsv(&mut updated, &tsv);
        }
    }
    if let Ok(output) = std::process::Command::new("/home/simon/projects/herdr-night-watch/tools/fetch_glm.sh")
        .output()
    {
        if output.status.success() {
            let json = String::from_utf8_lossy(&output.stdout);
            update_snapshot_with_glm_json(&mut updated, &json);
        }
    }
    let codex_json = std::process::Command::new("python3")
        .arg("/home/simon/projects/herdr-night-watch/tools/fetch_codex_quota.py")
        .output().ok().filter(|output| output.status.success());
    update_snapshot_with_codex_json(
        &mut updated,
        codex_json.as_ref().map(|output| output.stdout.as_slice())
            .and_then(|bytes| std::str::from_utf8(bytes).ok()).unwrap_or("null"),
    );
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_snapshot_baseline() {
        let snapshot = QuotaSnapshot::measured_baseline();
        assert_eq!(snapshot.providers.len(), 4);
        assert!(!snapshot.has_throttle);

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
            r#"{"week":{"remaining_percent":98,"reset":"30.09. (19:44)"},"five_hour":{"remaining_percent":0,"reset":"23:00"}}"#,
        );
        let codex = snapshot.get(ProviderId::Codex).unwrap();
        assert_eq!(codex.week_percent, Some(98));
        assert_eq!(codex.five_hour_percent, Some(0));
        assert!(snapshot.has_throttle);
        assert!(codex.pacing_forecast().five_hour_forecast.is_some());

        update_snapshot_with_codex_json(&mut snapshot, "null");
        let codex = snapshot.get(ProviderId::Codex).unwrap();
        assert_eq!(codex.week_percent, None);
        assert_eq!(codex.five_hour_percent, None);
        assert_eq!(codex.week_reset, None);
        assert!(!snapshot.has_throttle);
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

    #[test]
    fn test_update_snapshot_with_agy_tsv() {
        let mut snapshot = QuotaSnapshot::measured_baseline();
        let tsv = "Gemini Models\tFive Hour Limit Remaining\t92%\t2026-09-19T13:05:01Z\nGemini Models\tWeekly Limit Remaining\t89%\t2026-09-25T10:53:16Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-09-26T08:25:40Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-09-19T13:25:40Z";
        update_snapshot_with_agy_tsv(&mut snapshot, tsv);
        let agi = snapshot.get(ProviderId::Agy).expect("AGI exists");
        assert_eq!(agi.five_hour_percent, Some(92));
        assert_eq!(agi.week_percent, Some(89));
        assert_eq!(agi.week_reset, Some("25.09. (12:53)".to_string()));
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
        assert_eq!(week_de, "Reicht noch 4.9 Tage (27.09)");
        let week_en = fc.week_runway_text(Language::English);
        assert_eq!(week_en, "Lasts 4.9 days (27.09)");
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
    fn test_format_reset_time_phrase_tomorrow_and_over_weekend() {
        let now_dt = (2026, 9, 23, 12, 36); // Wednesday noon

        // Tomorrow at 19:00 -> in ~30h
        let phrase_tomorrow = format_reset_time_phrase(Some("24.09. (19:00)"), false, Language::German, now_dt);
        assert_eq!(phrase_tomorrow, "in ~30h (24.09. 19:00)");

        // Friday at 12:53 -> in ~48h
        let phrase_fri = format_reset_time_phrase(Some("25.09. (12:53)"), false, Language::German, now_dt);
        assert_eq!(phrase_fri, "in ~48h (25.09. 12:53)");

        // Over the weekend to Monday 19:00 -> in ~126h
        let phrase_mon = format_reset_time_phrase(Some("28.09. (19:00)"), false, Language::German, now_dt);
        assert_eq!(phrase_mon, "in ~126h (28.09. 19:00)");
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
}
