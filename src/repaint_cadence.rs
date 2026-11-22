//! Repaint cadence of the live-status windows.
//!
//! Every rendered frame grows the eframe/egui_glow host heap a little on a
//! locked desktop (forensic evidence, 22.11.2026), so the cadence is the one
//! authority that decides how often frames happen. The user's interval stays
//! untouched while the session is unlocked; while it is locked the windows are
//! invisible, so a calm interval replaces it. Anything time-critical that must
//! stay readable on the locked screen (shutdown countdown, pomodoro timer)
//! keeps a faster locked interval, and a caller's already calmer (longer)
//! base interval is never shortened or accelerated.

/// Repaint interval while the session is locked and nothing time-critical runs.
pub const LOCKED_REPAINT_INTERVAL_MS: u32 = 15_000;
/// Repaint interval while the session is locked and a countdown must stay fresh.
pub const LOCKED_COUNTDOWN_REPAINT_INTERVAL_MS: u32 = 5_000;
/// Repaint interval while a pomodoro timer runs (seconds must tick visibly).
pub const POMODORO_REPAINT_INTERVAL_MS: u32 = 100;

/// The one cadence decision for the live-status windows.
///
/// Unlocked: the base interval applies as-is. Locked: at least the calm locked
/// interval, or the faster countdown interval when `has_active_countdown` is
/// true — but never shorter than the caller's own base interval, so an
/// already calmer (longer) configuration (or the pomodoro cadence) is
/// preserved.
pub fn effective_repaint_interval_ms(
    base_interval_ms: u32,
    session_locked: bool,
    has_active_countdown: bool,
) -> u32 {
    if !session_locked {
        return base_interval_ms;
    }
    let locked_floor = if has_active_countdown {
        LOCKED_COUNTDOWN_REPAINT_INTERVAL_MS
    } else {
        LOCKED_REPAINT_INTERVAL_MS
    };
    base_interval_ms.max(locked_floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocked_uses_the_base_interval_verbatim() {
        for base in [250u32, 500, 1000] {
            assert_eq!(effective_repaint_interval_ms(base, false, false), base);
            assert_eq!(effective_repaint_interval_ms(base, false, true), base);
        }
    }

    #[test]
    fn pomodoro_runs_at_its_own_cadence_while_unlocked() {
        assert_eq!(
            effective_repaint_interval_ms(POMODORO_REPAINT_INTERVAL_MS, false, false),
            100
        );
    }

    #[test]
    fn locked_without_countdown_falls_back_to_the_calm_interval() {
        assert_eq!(effective_repaint_interval_ms(250, true, false), 15_000);
    }

    #[test]
    fn locked_with_shutdown_countdown_keeps_a_fresh_interval() {
        assert_eq!(effective_repaint_interval_ms(250, true, true), 5_000);
    }

    #[test]
    fn locked_pomodoro_keeps_the_countdown_interval() {
        assert_eq!(
            effective_repaint_interval_ms(POMODORO_REPAINT_INTERVAL_MS, true, true),
            5_000
        );
    }

    #[test]
    fn never_shortens_a_slower_base_interval() {
        assert_eq!(effective_repaint_interval_ms(20_000, true, false), 20_000);
        assert_eq!(effective_repaint_interval_ms(60_000, true, true), 60_000);
    }
}
