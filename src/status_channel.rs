//! One bounded, persistent status-only child per UI process.
use anyhow::{Result, anyhow, bail};
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
