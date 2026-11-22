//! One bounded, persistent status-only child per UI process.
use crate::language::Language;
use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(26); // 3.2 seconds remain for cleanup.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
// After an unverified helper shutdown (e.g. the transport child lost its Linux
// side to a WSL restart and never confirmed EOF), pause briefly instead of
// stopping permanently: a fresh spawn one minute later is safe and lets the
// tray survive WSL restarts without a manual app restart.
const RECOVERY_PAUSE: Duration = Duration::from_secs(60);
#[derive(Default)]
pub struct InputState {
    pub writer: Option<ChildStdin>,
    pub closing: bool,
}
pub type Input = Arc<Mutex<InputState>>;

pub struct Channel {
    session: Option<Session>,
    key: String,
    id: u64,
    failures: u32,
    retry_at: Instant,
    cleanup_failed: bool,
    cleanup_retry_at: Instant,
    pub input: Input,
}
struct Session {
    child: Child,
    frames: mpsc::Receiver<Result<Value, String>>,
    sequence: u64,
}
impl Channel {
    pub fn new(input: Input) -> Self {
        Self {
            session: None,
            key: String::new(),
            id: 0,
            failures: 0,
            retry_at: Instant::now(),
            cleanup_failed: false,
            cleanup_retry_at: Instant::now(),
            input,
        }
    }
    pub fn request(&mut self, key: String, command: Command) -> Result<Value> {
        self.request_with_timeout(key, command, REQUEST_TIMEOUT)
    }
    fn request_with_timeout(
        &mut self,
        key: String,
        mut command: Command,
        timeout: Duration,
    ) -> Result<Value> {
        if key != self.key {
            self.close();
            self.key = key;
            self.failures = 0;
            self.retry_at = Instant::now();
        }
        if self.cleanup_failed {
            if Instant::now() < self.cleanup_retry_at {
                bail!("Statuskanal pausiert nach unbestätigter Beendigung; Neuer Versuch in Kürze");
            }
            // The recovery pause has elapsed; give a fresh helper generation a
            // chance instead of requiring an app restart. A failed probe
            // re-arms the pause via close_until.
            self.cleanup_failed = false;
        }
        if Instant::now() < self.retry_at {
            bail!("Status unbekannt: Verbindung wartet vor erneutem Versuch");
        }
        let result = (|| {
            if self.session.is_none() {
                command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let mut input = self.input.lock().unwrap();
                if input.closing {
                    bail!("Statuskanal wird beendet");
                }
                let mut child = command.spawn()?;
                input.writer = child.stdin.take();
                drop(input);
                let stdout = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                // Continuously drain stderr using a fixed buffer.
                thread::spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    let mut buffer = [0; 4096];
                    while matches!(reader.read(&mut buffer), Ok(n) if n > 0) {}
                });
                let (tx, frames) = mpsc::sync_channel(2);
                thread::spawn(move || {
                    let mut reader = BufReader::new(stdout);
                    loop {
                        let mut line = Vec::new();
                        let result = (&mut reader)
                            .take((MAX_FRAME + 1) as u64)
                            .read_until(b'\n', &mut line);
                        let frame = match result {
                            Ok(0) => break,
                            Ok(_) if line.len() <= MAX_FRAME && line.ends_with(b"\n") => {
                                serde_json::from_slice(&line)
                                    .map_err(|e| format!("Ungültige Statusantwort: {e}"))
                            }
                            Ok(_) => Err("Statusantwort zu groß oder unvollständig".into()),
                            Err(e) => Err(e.to_string()),
                        };
                        let failed = frame.is_err();
                        if tx.try_send(frame).is_err() || failed {
                            break;
                        }
                    }
                });
                self.session = Some(Session {
                    child,
                    frames,
                    sequence: 0,
                });
            }
            self.id += 1;
            {
                let mut input = self.input.lock().unwrap();
                if input.closing {
                    bail!("Statuskanal wird beendet");
                }
                let writer = input
                    .writer
                    .as_mut()
                    .ok_or_else(|| anyhow!("Statuskanal geschlossen"))?;
                writeln!(
                    writer,
                    "{}",
                    json!({"version":1,"op":"status","id":self.id})
                )?;
                writer.flush()?;
            }
            let session = self.session.as_mut().unwrap();
            let response = session
                .frames
                .recv_timeout(timeout)
                .map_err(|e| anyhow!("Statuskanal antwortet nicht: {e}"))?
                .map_err(|e| anyhow!(e))?;
            if response["version"] != 1 || response["id"].as_u64() != Some(self.id) {
                bail!("Statusantwort passt nicht zur Anfrage");
            }
            if let Some(error) = response["error"].as_str() {
                bail!("Status unbekannt: {error}");
            }
            let sequence = response["sequence"]
                .as_u64()
                .ok_or_else(|| anyhow!("Statussequenz fehlt"))?;
            // Freshness is enforced by the request id and the strictly growing
            // sequence; comparing the Windows clock against the helper's WSL
            // wall clock would misfire for the whole session whenever WSL time
            // drifts after a host sleep.
            if response["generated_unix_ms"].as_u64().is_none() {
                bail!("Statuszeit fehlt");
            }
            if sequence <= session.sequence {
                bail!("Veraltete Statusantwort");
            }
            session.sequence = sequence;
            response
                .get("status")
                .cloned()
                .ok_or_else(|| anyhow!("Statusdaten fehlen"))
        })();
        if result.is_err() {
            self.close();
            self.failures += 1;
            // Exponential backoff capped at 64 seconds: repeated errors slow
            // retries down but never stop them permanently, so a WSL restart
            // under a long-running tray heals itself.
            self.retry_at = Instant::now() + Duration::from_secs(1 << (self.failures - 1).min(6));
        } else {
            self.failures = 0;
        }
        result
    }
    pub fn close(&mut self) {
        self.close_until(Instant::now() + CLEANUP_TIMEOUT);
    }
    pub fn close_until(&mut self, deadline: Instant) {
        self.input.lock().unwrap().writer.take(); // EOF cancels an in-flight request too.
        if let Some(mut session) = self.session.take() {
            let mut acknowledged = false;
            while Instant::now() < deadline {
                match session
                    .frames
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(Ok(frame)) if frame["version"] == 1 && frame["closed"] == true => {
                        acknowledged = true;
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            while Instant::now() < deadline {
                if matches!(session.child.try_wait(), Ok(Some(_))) {
                    // The helper ended without the closing frame (typical after
                    // a WSL restart); arm the recovery pause here as well so
                    // the next request waits instead of spawning immediately.
                    self.cleanup_failed |= !acknowledged;
                    if self.cleanup_failed {
                        self.cleanup_retry_at = Instant::now() + RECOVERY_PAUSE;
                    }
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = session.child.kill();
            // Reap our direct launcher; terminating it does not prove Linux cleanup.
            let reap_deadline = deadline + Duration::from_millis(200);
            while Instant::now() < reap_deadline {
                if matches!(session.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            if !matches!(session.child.try_wait(), Ok(Some(_))) {
                thread::spawn(move || {
                    let _ = session.child.wait();
                });
            }
            // Do not launch another Linux generation after unverified cleanup
            // immediately; allow one fresh probe after RECOVERY_PAUSE.
            self.cleanup_failed = true;
            self.cleanup_retry_at = Instant::now() + RECOVERY_PAUSE;
        }
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.close();
    }
}

/// One attention-filtered agent row (`attention_items` of `live_agent_summary()`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AttentionItem {
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub tab: String,
    #[serde(default)]
    pub status: String,
}

/// Attention fields of the watcher payload. Every field is optional on the wire:
/// an older watcher omits them, and `attention_total` stays `None`, so the fourth
/// KPI tile keeps its raw finished view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AttentionPayload {
    #[serde(default)]
    pub attention_available: bool,
    #[serde(default)]
    pub attention_done: Option<usize>,
    #[serde(default)]
    pub attention_blocked: Option<usize>,
    #[serde(default)]
    pub attention_total: Option<usize>,
    #[serde(default)]
    pub suppressed_done: Option<usize>,
    #[serde(default)]
    pub suppressed_blocked: Option<usize>,
    #[serde(default)]
    pub suppressed_unknown: Option<usize>,
    #[serde(default)]
    pub attention_items: Vec<AttentionItem>,
}

/// What the fourth KPI tile shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FourthTile {
    /// Only agents that need attention; `done` is the attention-filtered finished count.
    Attention {
        total: usize,
        done: usize,
        blocked: usize,
        suppressed_blocked: usize,
    },
    /// Raw view of an older watcher or a failed snapshot: all finished agents.
    Finished { done: usize },
}

/// Picks the fourth tile. The attention view needs `attention_available` and an
/// attention total; anything else falls back to the raw `done` count and never
/// shows a fake "Für dich 0".
pub fn fourth_tile(attention: &AttentionPayload, raw_done: usize) -> FourthTile {
    match (attention.attention_available, attention.attention_total) {
        (true, Some(total)) => FourthTile::Attention {
            total,
            done: attention.attention_done.unwrap_or(0),
            blocked: attention.attention_blocked.unwrap_or(0),
            suppressed_blocked: attention.suppressed_blocked.unwrap_or(0),
        },
        _ => FourthTile::Finished { done: raw_done },
    }
}

/// Which finished agents the fourth KPI tile counts. Only the tile and its tooltip depend on this mode;
/// the watcher's shutdown countdown checks the whole fleet and ignores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FourthTileMode {
    /// Current default: only finished or blocked managers, shown as "Für dich".
    #[default]
    ManagersOnly,
    /// Original mode: every finished agent, unfiltered, shown as "Fertig".
    AllAgents,
}

/// Reads the stored mode. Only "all" (trimmed, case-insensitive) selects the unfiltered view. A missing or
/// unknown value keeps the default, so a damaged registry entry never changes what the tile shows.
pub fn parse_fourth_tile_mode(raw: Option<&str>) -> FourthTileMode {
    match raw.map(|value| value.trim().eq_ignore_ascii_case("all")) {
        Some(true) => FourthTileMode::AllAgents,
        _ => FourthTileMode::ManagersOnly,
    }
}

/// The token written to the registry. It round-trips through `parse_fourth_tile_mode`.
pub fn fourth_tile_mode_token(mode: FourthTileMode) -> &'static str {
    match mode {
        FourthTileMode::ManagersOnly => "managers",
        FourthTileMode::AllAgents => "all",
    }
}

/// Picks the fourth tile for the chosen mode. `AllAgents` ignores the attention payload entirely.
pub fn fourth_tile_for_mode(attention: &AttentionPayload, raw_done: usize, mode: FourthTileMode) -> FourthTile {
    match mode {
        FourthTileMode::AllAgents => FourthTile::Finished { done: raw_done },
        FourthTileMode::ManagersOnly => fourth_tile(attention, raw_done),
    }
}

/// Whole-fleet counts that the attention tooltip reconciles against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FleetCounts {
    pub total: usize,
    pub working: usize,
    pub idle: usize,
    pub blocked: usize,
    pub unknown: usize,
}

/// Maximum number of attention rows listed in the tooltip.
pub const ATTENTION_TOOLTIP_ITEM_LIMIT: usize = 8;

/// Hover text of the KPI panel: the full arithmetic of the fleet, a warning line
/// for suppressed blocked helpers, and up to eight concrete attention rows.
pub fn attention_tooltip_lines(
    fleet: FleetCounts,
    attention: &AttentionPayload,
    language: Language,
) -> Vec<String> {
    let attention_total = attention.attention_total.unwrap_or(0);
    let suppressed_done = attention.suppressed_done.unwrap_or(0);
    let suppressed_blocked = attention.suppressed_blocked.unwrap_or(0);

    let mut summary = format!(
        "{} {}: {} {}, {} {}, {} {}, {} {}",
        fleet.total,
        language.text("erkannt", "detected"),
        fleet.working,
        language.text("arbeiten", "working"),
        fleet.idle,
        language.text("bereit", "ready"),
        attention_total,
        language.text("für dich", "for you"),
        suppressed_done,
        language.text("Zuarbeiter fertig", "helpers finished"),
    );
    // `fleet.blocked` enthält auch Agenten, die schon in „für dich“ (attention_blocked) oder in der ⚠-Zeile
    // (suppressed_blocked) stehen. Nur der Rest ist wirklich unklar; sonst erscheint ein Agent zweimal.
    let already_listed_blocked = attention.attention_blocked.unwrap_or(0) + suppressed_blocked;
    let unclear = fleet.blocked.saturating_sub(already_listed_blocked) + fleet.unknown;
    if unclear > 0 {
        summary.push_str(&format!(
            ", +{unclear} {}",
            language.text("blockiert/unklar", "blocked/unclear")
        ));
    }

    let mut lines = vec![summary];
    if suppressed_blocked > 0 {
        lines.push(format!(
            "⚠ {suppressed_blocked} {}",
            language.text("Zuarbeiter blockiert", "helpers blocked")
        ));
    }
    for item in attention.attention_items.iter().take(ATTENTION_TOOLTIP_ITEM_LIMIT) {
        let status = match item.status.as_str() {
            "done" => language.text("fertig", "finished").to_string(),
            "blocked" => language.text("blockiert", "blocked").to_string(),
            other => other.to_string(),
        };
        lines.push(format!("{} › {} · {}", item.workspace, item.tab, status));
    }
    lines
}

/// Hover text of the tile in the "Alle" mode: the plain fleet arithmetic with the raw finished count.
/// Blocked and unknown agents stay visible here, since this view has no attention list to show them in.
pub fn finished_tooltip_lines(fleet: FleetCounts, done: usize, language: Language) -> Vec<String> {
    let mut summary = format!(
        "{} {}: {} {}, {} {}, {} {}",
        fleet.total,
        language.text("erkannt", "detected"),
        fleet.working,
        language.text("arbeiten", "working"),
        fleet.idle,
        language.text("bereit", "ready"),
        done,
        language.text("fertig", "finished"),
    );
    let unclear = fleet.blocked + fleet.unknown;
    if unclear > 0 {
        summary.push_str(&format!(
            ", +{unclear} {}",
            language.text("blockiert/unklar", "blocked/unclear")
        ));
    }
    vec![summary]
}

#[cfg(test)]
mod attention_tests {
    use super::*;

    const OLD_PAYLOAD: &str =
        r#"{"available":true,"total":35,"working":12,"idle":6,"done":17,"blocked":0,"unknown":0}"#;

    const NEW_PAYLOAD: &str = r#"{
        "available": true, "total": 35, "working": 12, "idle": 6, "done": 17,
        "blocked": 0, "unknown": 0,
        "attention_available": true, "attention_done": 1, "attention_blocked": 0,
        "attention_total": 1, "suppressed_done": 16, "suppressed_blocked": 1,
        "suppressed_unknown": 0,
        "attention_items": [
            { "workspace": "🏛️ workspace-a · mother-v1", "tab": "mother-v1", "status": "done" }
        ]
    }"#;

    #[test]
    fn old_watcher_payload_falls_back_to_the_raw_finished_tile() {
        let attention: AttentionPayload = serde_json::from_str(OLD_PAYLOAD).expect("old payload");
        assert!(!attention.attention_available);
        assert_eq!(attention.attention_total, None);
        assert_eq!(fourth_tile(&attention, 17), FourthTile::Finished { done: 17 });
    }

    #[test]
    fn unavailable_or_incomplete_attention_never_shows_for_you_zero() {
        let mut guard_missing_total: AttentionPayload =
            serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        guard_missing_total.attention_total = None;
        assert_eq!(fourth_tile(&guard_missing_total, 17), FourthTile::Finished { done: 17 });

        let mut snapshot_failed: AttentionPayload =
            serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        snapshot_failed.attention_available = false;
        assert_eq!(fourth_tile(&snapshot_failed, 17), FourthTile::Finished { done: 17 });
    }

    #[test]
    fn attention_payload_shows_the_filtered_count_with_the_blocked_marker() {
        let attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        assert_eq!(
            fourth_tile(&attention, 17),
            FourthTile::Attention { total: 1, done: 1, blocked: 0, suppressed_blocked: 1 }
        );
    }

    #[test]
    fn zero_attention_is_a_calm_zero_not_a_fallback() {
        let mut attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        attention.attention_total = Some(0);
        attention.attention_done = Some(0);
        attention.attention_items.clear();
        assert_eq!(
            fourth_tile(&attention, 17),
            FourthTile::Attention { total: 0, done: 0, blocked: 0, suppressed_blocked: 1 }
        );
    }

    #[test]
    fn fourth_tile_mode_parses_defensively_and_round_trips() {
        assert_eq!(parse_fourth_tile_mode(Some("all")), FourthTileMode::AllAgents);
        assert_eq!(parse_fourth_tile_mode(Some(" ALL ")), FourthTileMode::AllAgents);
        assert_eq!(parse_fourth_tile_mode(Some("managers")), FourthTileMode::ManagersOnly);
        assert_eq!(parse_fourth_tile_mode(Some("unknown")), FourthTileMode::ManagersOnly);
        assert_eq!(parse_fourth_tile_mode(Some("")), FourthTileMode::ManagersOnly);
        assert_eq!(parse_fourth_tile_mode(None), FourthTileMode::ManagersOnly);
        for mode in [FourthTileMode::ManagersOnly, FourthTileMode::AllAgents] {
            assert_eq!(parse_fourth_tile_mode(Some(fourth_tile_mode_token(mode))), mode);
        }
    }

    #[test]
    fn all_agents_mode_ignores_the_attention_payload() {
        let attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        assert_eq!(
            fourth_tile_for_mode(&attention, 17, FourthTileMode::AllAgents),
            FourthTile::Finished { done: 17 }
        );
    }

    #[test]
    fn managers_only_mode_uses_the_attention_view_and_keeps_the_fallback() {
        let attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        assert_eq!(
            fourth_tile_for_mode(&attention, 17, FourthTileMode::ManagersOnly),
            FourthTile::Attention { total: 1, done: 1, blocked: 0, suppressed_blocked: 1 }
        );
        let old: AttentionPayload = serde_json::from_str(OLD_PAYLOAD).expect("old payload");
        assert_eq!(
            fourth_tile_for_mode(&old, 17, FourthTileMode::ManagersOnly),
            FourthTile::Finished { done: 17 }
        );
    }

    #[test]
    fn finished_tooltip_is_one_line_in_both_languages_and_names_unclear_agents() {
        let fleet = FleetCounts { total: 35, working: 12, idle: 6, blocked: 2, unknown: 1 };
        assert_eq!(
            finished_tooltip_lines(fleet, 17, Language::German),
            vec!["35 erkannt: 12 arbeiten, 6 bereit, 17 fertig, +3 blockiert/unklar".to_string()]
        );
        assert_eq!(
            finished_tooltip_lines(fleet, 17, Language::English),
            vec!["35 detected: 12 working, 6 ready, 17 finished, +3 blocked/unclear".to_string()]
        );
        let calm = FleetCounts { total: 35, working: 12, idle: 6, blocked: 0, unknown: 0 };
        assert_eq!(
            finished_tooltip_lines(calm, 17, Language::German),
            vec!["35 erkannt: 12 arbeiten, 6 bereit, 17 fertig".to_string()]
        );
    }

    #[test]
    fn german_tooltip_spells_out_the_arithmetic_and_the_items() {
        let attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        let fleet = FleetCounts { total: 35, working: 12, idle: 6, blocked: 0, unknown: 0 };
        let lines = attention_tooltip_lines(fleet, &attention, Language::German);
        assert_eq!(
            lines,
            vec![
                "35 erkannt: 12 arbeiten, 6 bereit, 1 für dich, 16 Zuarbeiter fertig".to_string(),
                "⚠ 1 Zuarbeiter blockiert".to_string(),
                "🏛️ workspace-a · mother-v1 › mother-v1 · fertig".to_string(),
            ]
        );
    }

    #[test]
    fn english_tooltip_adds_unclear_agents_and_limits_the_items() {
        let mut attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        attention.suppressed_blocked = Some(0);
        attention.attention_items = (0..10)
            .map(|index| AttentionItem {
                workspace: format!("ws{index}"),
                tab: "architekt".into(),
                status: "blocked".into(),
            })
            .collect();
        let fleet = FleetCounts { total: 35, working: 12, idle: 6, blocked: 2, unknown: 1 };
        let lines = attention_tooltip_lines(fleet, &attention, Language::English);
        assert_eq!(
            lines[0],
            "35 detected: 12 working, 6 ready, 1 for you, 16 helpers finished, +3 blocked/unclear"
        );
        assert_eq!(lines.len(), 1 + ATTENTION_TOOLTIP_ITEM_LIMIT);
        assert_eq!(lines[1], "ws0 › architekt · blocked");
    }

    #[test]
    fn blocked_attention_agents_are_not_counted_again_as_unclear() {
        let mut attention: AttentionPayload = serde_json::from_str(NEW_PAYLOAD).expect("new payload");
        attention.attention_blocked = Some(1);
        attention.suppressed_blocked = Some(0);
        // Zwei blockierte Agenten insgesamt, davon einer bereits als „für dich“ gezählt.
        let fleet = FleetCounts { total: 35, working: 12, idle: 6, blocked: 2, unknown: 0 };
        let lines = attention_tooltip_lines(fleet, &attention, Language::English);
        assert_eq!(
            lines[0],
            "35 detected: 12 working, 6 ready, 1 for you, 16 helpers finished, +1 blocked/unclear"
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn helper(body: &str) -> Command {
        let mut command = Command::new("python3");
        command.arg("-u").arg("-c").arg(format!(
            r#"
import json,sys,time,os
for line in sys.stdin:
 r=json.loads(line)
 {body}
print(json.dumps({{'version':1,'closed':True}}),flush=True)
"#
        ));
        command
    }
    fn channel() -> Channel {
        Channel::new(Default::default())
    }
    const VALID: &str = "print(json.dumps({'version':1,'id':r['id'],'sequence':r['id'],'generated_unix_ms':int(time.time()*1000),'status':{'pid':os.getpid()}}),flush=True)";
    #[test]
    fn shutdown_between_caller_check_and_spawn_prevents_child() {
        let input: Input = Default::default();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_input = input.clone();
        let worker_barrier = barrier.clone();
        let worker = thread::spawn(move || {
            let mut c = Channel::new(worker_input);
            worker_barrier.wait();
            assert!(c.request("a".into(), helper(VALID)).is_err());
            assert!(c.session.is_none());
        });
        input.lock().unwrap().closing = true;
        barrier.wait();
        worker.join().unwrap();
        assert!(input.lock().unwrap().writer.is_none());
    }
    #[test]
    fn repeated_requests_reuse_child_and_close_verified() {
        let mut c = channel();
        let first = c.request("a".into(), helper(VALID)).unwrap();
        for _ in 0..20 {
            assert_eq!(first, c.request("a".into(), helper(VALID)).unwrap());
        }
        c.close();
        assert!(!c.cleanup_failed);
        assert_eq!(c.failures, 0);
    }
    #[test]
    fn config_change_replaces_verified_generation() {
        let mut c = channel();
        let first = c.request("a".into(), helper(VALID)).unwrap();
        let second = c.request("b".into(), helper(VALID)).unwrap();
        assert_ne!(first, second);
        c.close();
        assert!(!c.cleanup_failed);
    }
    #[test]
    fn malformed_mismatched_stale_and_oversize_are_rejected() {
        for body in [
            "print('garbage',flush=True)",
            "print(json.dumps({'version':1,'id':999}),flush=True)",
            "print(json.dumps({'version':1,'id':r['id'],'sequence':0,'generated_unix_ms':int(time.time()*1000),'status':{}}),flush=True)",
            "print('x'*300000,flush=True)",
        ] {
            let mut c = channel();
            assert!(c.request("a".into(), helper(body)).is_err());
            assert_eq!(c.failures, 1);
        }
    }
    #[test]
    fn timeout_does_not_spawn_unverified_generation() {
        let mut c = channel();
        let start = Instant::now();
        assert!(
            c.request_with_timeout(
                "a".into(),
                helper("time.sleep(30)"),
                Duration::from_millis(50)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(c.cleanup_failed);
        assert!(c.request("b".into(), helper(VALID)).is_err());
    }
    #[test]
    fn unverified_cleanup_recovers_after_pause() {
        let mut c = channel();
        assert!(
            c.request_with_timeout(
                "a".into(),
                helper("time.sleep(30)"),
                Duration::from_millis(50)
            )
            .is_err()
        );
        assert!(c.cleanup_failed);
        // While paused, requests fail fast without spawning a new generation.
        assert!(c.request("a".into(), helper(VALID)).is_err());
        assert!(c.session.is_none());
        // Once the recovery pause has elapsed, a fresh helper generation is
        // allowed again and a healthy helper heals the channel.
        c.cleanup_retry_at = Instant::now() - Duration::from_secs(1);
        c.retry_at = Instant::now(); // Skip the backoff owed by the timeout above.
        assert!(c.request("a".into(), helper(VALID)).is_ok());
        assert!(!c.cleanup_failed);
        assert_eq!(c.failures, 0);
    }

    #[test]
    fn helper_exit_without_close_frame_arms_the_recovery_pause() {
        // The typical WSL-restart shape: wsl.exe ends without the closing
        // frame, so the request times out and cleanup is never acknowledged.
        let mut c = channel();
        assert!(
            c.request_with_timeout(
                "a".into(),
                helper("import sys; sys.stdout.close()"),
                Duration::from_millis(50)
            )
            .is_err()
        );
        assert!(c.cleanup_failed);
        // The pause must be armed in this branch as well, not only after a kill.
        assert!(c.cleanup_retry_at > Instant::now());
        assert!(c.request("a".into(), helper(VALID)).is_err());
        assert!(c.session.is_none());
    }
    #[test]
    fn repeated_errors_back_off_but_never_stop_permanently() {
        let mut c = channel();
        let failure = "print(json.dumps({'version':1,'id':r['id'],'error':'test'}),flush=True)";
        for n in 1..=5 {
            c.retry_at = Instant::now();
            assert!(c.request("a".into(), helper(failure)).is_err());
            assert_eq!(c.failures, n);
            assert!(!c.cleanup_failed);
            assert!(c.retry_at > Instant::now());
        }
        // Same key, no app restart: a healthy helper recovers the channel.
        c.retry_at = Instant::now();
        assert!(c.request("a".into(), helper(VALID)).is_ok());
        assert_eq!(c.failures, 0);
    }
}
