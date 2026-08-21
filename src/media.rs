use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
};
use std::thread;
use std::time::{Duration, Instant};

use pollster::block_on;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

const FALLBACK_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const RECOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TIMELINE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSnapshot {
    pub title: String,
    pub artist: String,
    pub start_100ns: i64,
    pub end_100ns: i64,
    pub position_100ns: i64,
    pub seek_enabled: bool,
    is_playing: bool,
    position_sampled_at: Instant,
}

impl MediaSnapshot {
    pub fn position_at(&self, elapsed: Duration) -> i64 {
        let elapsed_100ns = (elapsed.as_nanos() / 100).min(i64::MAX as u128) as i64;
        let position = if self.is_playing {
            self.position_100ns.saturating_add(elapsed_100ns)
        } else {
            self.position_100ns
        };
        position.clamp(self.start_100ns, self.end_100ns.max(self.start_100ns))
    }

    pub fn current_position_100ns(&self) -> i64 {
        self.position_at(
            Instant::now()
                .checked_duration_since(self.position_sampled_at)
                .unwrap_or_default(),
        )
    }
}

#[derive(Clone, Debug)]
pub enum MediaCommand {
    Seek(i64),
    Event,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshCause {
    Immediate,
    Timeline,
}

#[derive(Clone)]
struct RefreshNotifier {
    command_tx: Sender<MediaCommand>,
    wake_pending: Arc<AtomicBool>,
    immediate_pending: Arc<AtomicBool>,
    timeline_pending: Arc<AtomicBool>,
}

impl RefreshNotifier {
    fn new(command_tx: Sender<MediaCommand>) -> Self {
        Self {
            command_tx,
            wake_pending: Arc::new(AtomicBool::new(false)),
            immediate_pending: Arc::new(AtomicBool::new(false)),
            timeline_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn notify_immediate(&self) {
        self.immediate_pending.store(true, Ordering::SeqCst);
        self.wake();
    }

    fn notify_timeline(&self) {
        self.timeline_pending.store(true, Ordering::SeqCst);
        self.wake();
    }

    fn take_pending(&self) -> Option<RefreshCause> {
        self.wake_pending.store(false, Ordering::SeqCst);
        if self.immediate_pending.swap(false, Ordering::SeqCst) {
            Some(RefreshCause::Immediate)
        } else if self.timeline_pending.swap(false, Ordering::SeqCst) {
            Some(RefreshCause::Timeline)
        } else {
            None
        }
    }

    fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::SeqCst) {
            let _ = self.command_tx.send(MediaCommand::Event);
        }
    }
}

pub fn spawn_worker() -> (
    Sender<MediaCommand>,
    Receiver<Result<Option<MediaSnapshot>, String>>,
) {
    let (command_tx, command_rx) = mpsc::channel();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let notifier = RefreshNotifier::new(command_tx.clone());
    thread::spawn(move || run_worker(command_rx, notifier, snapshot_tx));
    (command_tx, snapshot_rx)
}

fn run_worker(
    command_rx: Receiver<MediaCommand>,
    notifier: RefreshNotifier,
    snapshot_tx: Sender<Result<Option<MediaSnapshot>, String>>,
) {
    // The Windows Runtime APIs need a COM apartment on the worker thread.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let manager = loop {
        match request_manager() {
            Ok(manager) => break manager,
            Err(error) => {
                let _ = snapshot_tx.send(Err(error));
                match command_rx.recv_timeout(RECOVERY_REFRESH_INTERVAL) {
                    Ok(MediaCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(MediaCommand::Seek(_)) => {
                        let _ = snapshot_tx.send(Err("Keine aktive Mediensession".into()));
                    }
                    Ok(MediaCommand::Event) => {
                        let _ = notifier.take_pending();
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    };

    let _manager_events = ManagerEventSubscriptions::new(&manager, notifier.clone());
    let mut active_session = None;
    let mut next_fallback =
        Instant::now() + refresh(&manager, &notifier, &snapshot_tx, &mut active_session);
    let mut pending_refresh = None;
    let mut last_event_refresh = Instant::now() - EVENT_REFRESH_INTERVAL;
    let mut last_timeline_refresh = Instant::now() - TIMELINE_REFRESH_INTERVAL;

    loop {
        let timeout = next_timeout(next_fallback, pending_refresh);
        match command_rx.recv_timeout(timeout) {
            Ok(MediaCommand::Seek(position)) => {
                let result = active_session
                    .as_ref()
                    .ok_or_else(|| "Keine aktive Mediensession".to_owned())
                    .and_then(|session| seek(&session.session, position));
                if let Err(error) = result {
                    let _ = snapshot_tx.send(Err(error));
                }
                next_fallback = Instant::now()
                    + refresh(&manager, &notifier, &snapshot_tx, &mut active_session);
                pending_refresh = None;
            }
            Ok(MediaCommand::Event) => {
                if let Some(cause) = notifier.take_pending() {
                    pending_refresh = merge_refresh(
                        pending_refresh,
                        cause,
                        last_event_refresh,
                        last_timeline_refresh,
                    );
                }
            }
            Ok(MediaCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                if let Some(pending) = pending_refresh.filter(|pending| pending.due_at <= now) {
                    next_fallback =
                        now + refresh(&manager, &notifier, &snapshot_tx, &mut active_session);
                    match pending.cause {
                        RefreshCause::Immediate => last_event_refresh = now,
                        RefreshCause::Timeline => last_timeline_refresh = now,
                    }
                    pending_refresh = None;
                } else if next_fallback <= now {
                    next_fallback =
                        now + refresh(&manager, &notifier, &snapshot_tx, &mut active_session);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct PendingRefresh {
    cause: RefreshCause,
    due_at: Instant,
}

fn merge_refresh(
    pending: Option<PendingRefresh>,
    cause: RefreshCause,
    last_event_refresh: Instant,
    last_timeline_refresh: Instant,
) -> Option<PendingRefresh> {
    let due_at = match cause {
        RefreshCause::Immediate => last_event_refresh + EVENT_REFRESH_INTERVAL,
        RefreshCause::Timeline => last_timeline_refresh + TIMELINE_REFRESH_INTERVAL,
    };
    match pending {
        Some(existing) if existing.cause == RefreshCause::Immediate => Some(existing),
        Some(_) if cause == RefreshCause::Timeline => pending,
        _ => Some(PendingRefresh { cause, due_at }),
    }
}

fn next_timeout(next_fallback: Instant, pending: Option<PendingRefresh>) -> Duration {
    let deadline = pending
        .map(|pending| pending.due_at.min(next_fallback))
        .unwrap_or(next_fallback);
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
}

fn request_manager() -> Result<GlobalSystemMediaTransportControlsSessionManager, String> {
    block_on(
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn refresh(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    notifier: &RefreshNotifier,
    snapshot_tx: &Sender<Result<Option<MediaSnapshot>, String>>,
    active_session: &mut Option<SessionEventSubscriptions>,
) -> Duration {
    match select_snapshot(manager) {
        Ok(Some((session, snapshot))) => {
            let session_changed = active_session
                .as_ref()
                .is_none_or(|active| active.session != session);
            if session_changed {
                *active_session = Some(SessionEventSubscriptions::new(session, notifier.clone()));
            }
            let _ = snapshot_tx.send(Ok(Some(snapshot)));
            FALLBACK_REFRESH_INTERVAL
        }
        Ok(None) => {
            *active_session = None;
            let _ = snapshot_tx.send(Ok(None));
            FALLBACK_REFRESH_INTERVAL
        }
        Err(error) => {
            let _ = snapshot_tx.send(Err(error));
            RECOVERY_REFRESH_INTERVAL
        }
    }
}

fn select_snapshot(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
) -> Result<Option<(GlobalSystemMediaTransportControlsSession, MediaSnapshot)>, String> {
    let mut had_session = false;
    let mut last_error = None;
    if let Ok(current) = manager.GetCurrentSession() {
        had_session = true;
        match read_session(&current) {
            Ok(snapshot) => return Ok(Some((current, snapshot))),
            Err(error) => last_error = Some(error),
        }
    }
    let sessions = manager.GetSessions().map_err(|error| error.to_string())?;
    let size = sessions.Size().map_err(|error| error.to_string())?;
    for index in 0..size {
        let session = sessions.GetAt(index).map_err(|error| error.to_string())?;
        had_session = true;
        match read_session(&session) {
            Ok(snapshot) => return Ok(Some((session, snapshot))),
            Err(error) => last_error = Some(error),
        }
    }
    if had_session {
        Err(last_error.unwrap_or_else(|| "Mediensession nicht lesbar".into()))
    } else {
        Ok(None)
    }
}

struct ManagerEventSubscriptions {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    current_session_changed: Option<i64>,
    sessions_changed: Option<i64>,
}

impl ManagerEventSubscriptions {
    fn new(
        manager: &GlobalSystemMediaTransportControlsSessionManager,
        notifier: RefreshNotifier,
    ) -> Self {
        let current_notifier = notifier.clone();
        let current_session_changed = manager
            .CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
                current_notifier.notify_immediate();
                Ok(())
            }))
            .ok();
        let sessions_changed = manager
            .SessionsChanged(&TypedEventHandler::new(move |_, _| {
                notifier.notify_immediate();
                Ok(())
            }))
            .ok();
        Self {
            manager: manager.clone(),
            current_session_changed,
            sessions_changed,
        }
    }
}

impl Drop for ManagerEventSubscriptions {
    fn drop(&mut self) {
        if let Some(token) = self.current_session_changed {
            let _ = self.manager.RemoveCurrentSessionChanged(token);
        }
        if let Some(token) = self.sessions_changed {
            let _ = self.manager.RemoveSessionsChanged(token);
        }
    }
}

struct SessionEventSubscriptions {
    session: GlobalSystemMediaTransportControlsSession,
    media_properties_changed: Option<i64>,
    playback_info_changed: Option<i64>,
    timeline_properties_changed: Option<i64>,
}

impl SessionEventSubscriptions {
    fn new(session: GlobalSystemMediaTransportControlsSession, notifier: RefreshNotifier) -> Self {
        let media_notifier = notifier.clone();
        let media_properties_changed = session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                media_notifier.notify_immediate();
                Ok(())
            }))
            .ok();
        let playback_notifier = notifier.clone();
        let playback_info_changed = session
            .PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                playback_notifier.notify_immediate();
                Ok(())
            }))
            .ok();
        let timeline_properties_changed = session
            .TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                notifier.notify_timeline();
                Ok(())
            }))
            .ok();
        Self {
            session,
            media_properties_changed,
            playback_info_changed,
            timeline_properties_changed,
        }
    }
}

impl Drop for SessionEventSubscriptions {
    fn drop(&mut self) {
        if let Some(token) = self.media_properties_changed {
            let _ = self.session.RemoveMediaPropertiesChanged(token);
        }
        if let Some(token) = self.playback_info_changed {
            let _ = self.session.RemovePlaybackInfoChanged(token);
        }
        if let Some(token) = self.timeline_properties_changed {
            let _ = self.session.RemoveTimelinePropertiesChanged(token);
        }
    }
}

fn read_session(
    session: &GlobalSystemMediaTransportControlsSession,
) -> Result<MediaSnapshot, String> {
    let properties = session
        .TryGetMediaPropertiesAsync()
        .ok()
        .and_then(|operation| block_on(operation).ok());
    let title = properties
        .as_ref()
        .and_then(|properties| properties.Title().ok())
        .map(|value| value.to_string())
        .unwrap_or_default();
    let artist = properties
        .as_ref()
        .and_then(|properties| properties.Artist().ok())
        .map(|value| value.to_string())
        .unwrap_or_default();
    if title.trim().is_empty() && artist.trim().is_empty() {
        return Err("Mediensession ohne Titel".into());
    }
    let timeline = session.GetTimelineProperties().ok();
    let start_100ns = timeline
        .as_ref()
        .and_then(|timeline| timeline.StartTime().ok())
        .map(|time| time.Duration)
        .unwrap_or_default();
    let end_100ns = timeline
        .as_ref()
        .and_then(|timeline| timeline.EndTime().ok())
        .map(|time| time.Duration)
        .unwrap_or_default();
    let position_100ns = timeline
        .as_ref()
        .and_then(|timeline| timeline.Position().ok())
        .map(|time| time.Duration)
        .unwrap_or(start_100ns)
        .clamp(start_100ns, end_100ns.max(start_100ns));
    let playback_info = session.GetPlaybackInfo().ok();
    let seek_enabled = playback_info
        .as_ref()
        .and_then(|info| info.Controls().ok())
        .and_then(|controls| controls.IsPlaybackPositionEnabled().ok())
        .unwrap_or(false)
        && end_100ns > start_100ns;
    let is_playing = playback_info
        .as_ref()
        .and_then(|info| info.PlaybackStatus().ok())
        == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    Ok(MediaSnapshot {
        title: if title.trim().is_empty() {
            "Unbekannter Titel".into()
        } else {
            title
        },
        artist,
        start_100ns,
        end_100ns,
        position_100ns,
        seek_enabled,
        is_playing,
        position_sampled_at: Instant::now(),
    })
}

fn seek(
    session: &GlobalSystemMediaTransportControlsSession,
    position_100ns: i64,
) -> Result<(), String> {
    let operation = session
        .TryChangePlaybackPositionAsync(position_100ns)
        .map_err(|error| error.to_string())?;
    let _ = block_on(operation).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_timeline_advances_without_a_windows_query() {
        let snapshot = MediaSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            start_100ns: 0,
            end_100ns: 300_000_000,
            position_100ns: 10_000_000,
            seek_enabled: true,
            is_playing: true,
            position_sampled_at: Instant::now(),
        };

        assert_eq!(snapshot.position_at(Duration::from_secs(2)), 30_000_000);
    }

    #[test]
    fn paused_timeline_stays_at_its_last_known_position() {
        let snapshot = MediaSnapshot {
            title: "Song".into(),
            artist: "Artist".into(),
            start_100ns: 0,
            end_100ns: 300_000_000,
            position_100ns: 10_000_000,
            seek_enabled: true,
            is_playing: false,
            position_sampled_at: Instant::now(),
        };

        assert_eq!(snapshot.position_at(Duration::from_secs(2)), 10_000_000);
    }

    #[test]
    fn notifier_coalesces_repeated_timeline_events() {
        let (tx, rx) = mpsc::channel();
        let notifier = RefreshNotifier::new(tx);

        notifier.notify_timeline();
        notifier.notify_timeline();

        assert!(matches!(rx.recv(), Ok(MediaCommand::Event)));
        assert!(rx.try_recv().is_err());
        assert_eq!(notifier.take_pending(), Some(RefreshCause::Timeline));
    }
}
