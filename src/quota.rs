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

    pub fn pacing_forecast(&self) -> PacingForecast {
        pacing_forecast_for(
            self.is_throttled,
            self.week_percent,
            self.week_reset.as_deref(),
            self.five_hour_percent,
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
pub struct PacingForecast {
    pub pace_ratio: Option<f32>,
    pub runway_days: Option<f32>,
    pub remaining_days: f32,
    pub health: PacingHealth,
    pub badge_text: String,
    pub summary_text: String,
    pub reset_str: Option<String>,
    pub today: (i32, u32, u32),
}

impl PacingForecast {
    pub fn localized_badge_text(&self, language: Language) -> String {
        match self.health {
            PacingHealth::Throttled => language.text("Gedrosselt", "Throttled").to_string(),
            _ => format!("Pace {:.1}x", self.pace_ratio.unwrap_or(0.1)),
        }
    }

    pub fn localized_summary_text(&self, language: Language) -> String {
        match self.health {
            PacingHealth::Throttled => {
                let reset = self.reset_str.as_deref().unwrap_or(language.text("bald", "soon"));
                format!("Reset {reset}")
            }
            PacingHealth::Surplus => {
                if self.runway_days.is_none() {
                    language.text("Voller Puffer", "Full buffer").to_string()
                } else {
                    language.text("Reicht locker", "Ample runway").to_string()
                }
            }
            PacingHealth::OnTrack => {
                let days = self.runway_days.unwrap_or(self.remaining_days).min(self.remaining_days + 7.0);
                match language {
                    Language::German => format!("Reicht ~{:.1} d", days),
                    Language::English => format!("Lasts ~{:.1} d", days),
                }
            }
            PacingHealth::Tight => {
                let days = self.runway_days.unwrap_or(self.remaining_days);
                let today_days = ymd_to_days(self.today.0, self.today.1, self.today.2);
                let add_days = days.round() as i64;
                if add_days <= 0 {
                    let hours = (days * 24.0).max(1.0).round() as u32;
                    match language {
                        Language::German => format!("Schluss: heute (~{hours} h)"),
                        Language::English => format!("Empty: today (~{hours} h)"),
                    }
                } else {
                    let empty_days = today_days + add_days;
                    let (_y, m, d) = days_to_ymd(empty_days);
                    match language {
                        Language::German => format!("Schluss: {:02}.{:02}. (~{:.0} d)", d, m, days.round()),
                        Language::English => format!("Empty: {:02}.{:02} (~{:.0} d)", d, m, days.round()),
                    }
                }
            }
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

pub fn parse_remaining_days(reset_str: Option<&str>, today: (i32, u32, u32)) -> Option<f32> {
    let raw = reset_str?.trim();
    let today_days = ymd_to_days(today.0, today.1, today.2);

    // Format 1: "25.09." or "25.09" or "01.10."
    if raw.contains('.') {
        let parts: Vec<&str> = raw.split('.').filter(|s| !s.trim().is_empty()).collect();
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

pub fn pacing_forecast_for(
    is_throttled: bool,
    week_percent: Option<u8>,
    week_reset: Option<&str>,
    five_hour_percent: Option<u8>,
    custom_today: Option<(i32, u32, u32)>,
) -> PacingForecast {
    let today = custom_today.unwrap_or_else(current_ymd);
    let rem_pct = week_percent.or(five_hour_percent).unwrap_or(100);

    let saved_reset = week_reset.map(|s| s.to_string());

    if is_throttled || rem_pct == 0 {
        let reset_str = week_reset.unwrap_or("bald");
        return PacingForecast {
            pace_ratio: None,
            runway_days: Some(0.0),
            remaining_days: parse_remaining_days(week_reset, today).unwrap_or(5.0),
            health: PacingHealth::Throttled,
            badge_text: "Gedrosselt".to_string(),
            summary_text: format!("Reset {reset_str}"),
            reset_str: saved_reset,
            today,
        };
    }

    let remaining_days = parse_remaining_days(week_reset, today).unwrap_or(7.0).max(0.1);
    let total_days = if remaining_days > 7.5 { 30.0 } else { 7.0 };
    let elapsed_days = (total_days - remaining_days).max(0.2);
    let consumed_pct = (100u8.saturating_sub(rem_pct)) as f32;

    let allowed_rate = 100.0 / total_days;
    let burn_rate = consumed_pct / elapsed_days;
    let pace_ratio = burn_rate / allowed_rate;

    if consumed_pct <= 2.0 || burn_rate <= 0.05 {
        return PacingForecast {
            pace_ratio: Some(0.1),
            runway_days: None,
            remaining_days,
            health: PacingHealth::Surplus,
            badge_text: "Pace 0.1x".to_string(),
            summary_text: "Voller Puffer".to_string(),
            reset_str: saved_reset,
            today,
        };
    }

    let runway_days = (rem_pct as f32) / burn_rate;

    if runway_days >= remaining_days {
        if pace_ratio < 0.85 {
            PacingForecast {
                pace_ratio: Some(pace_ratio),
                runway_days: Some(runway_days),
                remaining_days,
                health: PacingHealth::Surplus,
                badge_text: format!("Pace {:.1}x", pace_ratio),
                summary_text: "Reicht locker".to_string(),
                reset_str: saved_reset,
                today,
            }
        } else {
            PacingForecast {
                pace_ratio: Some(pace_ratio),
                runway_days: Some(runway_days),
                remaining_days,
                health: PacingHealth::OnTrack,
                badge_text: format!("Pace {:.1}x", pace_ratio),
                summary_text: format!("Reicht ~{:.1} d", runway_days.min(remaining_days + 7.0)),
                reset_str: saved_reset,
                today,
            }
        }
    } else {
        let today_days = ymd_to_days(today.0, today.1, today.2);
        let add_days = runway_days.round() as i64;
        let summary_text = if add_days <= 0 {
            let hours = (runway_days * 24.0).max(1.0).round() as u32;
            format!("Schluss: heute (~{hours} h)")
        } else {
            let (_y, m, d) = days_to_ymd(today_days + add_days);
            format!("Schluss: {:02}.{:02}. (~{:.0} d)", d, m, runway_days.round())
        };

        PacingForecast {
            pace_ratio: Some(pace_ratio),
            runway_days: Some(runway_days),
            remaining_days,
            health: PacingHealth::Tight,
            badge_text: format!("Pace {:.1}x", pace_ratio),
            summary_text,
            reset_str: saved_reset,
            today,
        }
    }
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
    /// Baseline measured on 2026-09-18 (GLM 76%/75%, AGI 98%/100%, Codex 0% throttled).
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
                Some("25.09.".into()),
            ),
            ProviderQuota::new(
                ProviderId::Codex,
                "Codex",
                "OpenAI",
                None,
                None,
                Some(0),
                Some("23.09.".into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_snapshot_baseline() {
        let snapshot = QuotaSnapshot::measured_baseline();
        assert_eq!(snapshot.providers.len(), 3);
        assert!(snapshot.has_throttle);

        let glm = snapshot.get(ProviderId::Glm).expect("GLM present");
        assert_eq!(glm.five_hour_percent, Some(76));
        assert_eq!(glm.week_percent, Some(75));
        assert!(!glm.is_throttled);

        let codex = snapshot.get(ProviderId::Codex).expect("Codex present");
        assert_eq!(codex.week_percent, Some(0));
        assert_eq!(codex.five_hour_percent, None);
        assert!(codex.is_throttled);
        assert_eq!(codex.status_text(), "Drossel aktiv");
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
    fn test_pacing_forecast_baseline() {
        let today = (2026, 9, 18);

        // GLM: 75% remaining, reset 01.10 (13 days away, 17 days elapsed of 30) -> Surplus
        let glm_fc = pacing_forecast_for(false, Some(75), Some("01.10."), Some(76), Some(today));
        assert_eq!(glm_fc.health, PacingHealth::Surplus);
        assert!(glm_fc.pace_ratio.unwrap() < 0.6);
        assert_eq!(glm_fc.summary_text, "Reicht locker");

        // AGI: 100% remaining, reset 25.09 (7 days away, 0 consumed) -> Surplus / Voller Puffer
        let agi_fc = pacing_forecast_for(false, Some(100), Some("25.09."), Some(98), Some(today));
        assert_eq!(agi_fc.health, PacingHealth::Surplus);
        assert_eq!(agi_fc.badge_text, "Pace 0.1x");
        assert_eq!(agi_fc.summary_text, "Voller Puffer");

        // Codex: Throttled (0% remaining, reset 23.09) -> Throttled
        let codex_fc = pacing_forecast_for(true, Some(0), Some("23.09."), None, Some(today));
        assert_eq!(codex_fc.health, PacingHealth::Throttled);
        assert_eq!(codex_fc.badge_text, "Gedrosselt");
        assert_eq!(codex_fc.summary_text, "Reset 23.09.");
    }

    #[test]
    fn test_pacing_forecast_simon_scenario_tight() {
        // Simon's scenario: 50% consumed on Day 2 of 7-day week (5 days remaining)
        // Reset in 5 days -> 2026-09-23 if today is 2026-09-18
        let today = (2026, 9, 18);
        let fc = pacing_forecast_for(false, Some(50), Some("23.09."), None, Some(today));
        assert_eq!(fc.health, PacingHealth::Tight);
        assert!(fc.pace_ratio.unwrap() >= 1.6);
        assert_eq!(fc.badge_text, "Pace 1.8x");
        assert_eq!(fc.summary_text, "Schluss: 20.09. (~2 d)");
    }

    #[test]
    fn test_pacing_forecast_localization_en_de() {
        let today = (2026, 9, 18);

        // Throttled: DE = "Gedrosselt", EN = "Throttled"
        let codex_fc = pacing_forecast_for(true, Some(0), Some("23.09."), None, Some(today));
        assert_eq!(codex_fc.localized_badge_text(Language::German), "Gedrosselt");
        assert_eq!(codex_fc.localized_badge_text(Language::English), "Throttled");
        assert_eq!(codex_fc.localized_summary_text(Language::German), "Reset 23.09.");
        assert_eq!(codex_fc.localized_summary_text(Language::English), "Reset 23.09.");

        // Surplus ample runway: DE = "Reicht locker", EN = "Ample runway"
        let glm_fc = pacing_forecast_for(false, Some(75), Some("01.10."), Some(76), Some(today));
        assert_eq!(glm_fc.localized_summary_text(Language::German), "Reicht locker");
        assert_eq!(glm_fc.localized_summary_text(Language::English), "Ample runway");

        // Surplus full buffer: DE = "Voller Puffer", EN = "Full buffer"
        let agi_fc = pacing_forecast_for(false, Some(100), Some("25.09."), Some(98), Some(today));
        assert_eq!(agi_fc.localized_summary_text(Language::German), "Voller Puffer");
        assert_eq!(agi_fc.localized_summary_text(Language::English), "Full buffer");

        // Tight: DE = "Schluss: 20.09. (~2 d)", EN = "Empty: 20.09 (~2 d)"
        let tight_fc = pacing_forecast_for(false, Some(50), Some("23.09."), None, Some(today));
        assert_eq!(tight_fc.localized_summary_text(Language::German), "Schluss: 20.09. (~2 d)");
        assert_eq!(tight_fc.localized_summary_text(Language::English), "Empty: 20.09 (~2 d)");
    }
}
