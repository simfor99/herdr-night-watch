use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use pollster::block_on;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSnapshot {
    pub title: String,
    pub artist: String,
    pub start_100ns: i64,
    pub end_100ns: i64,
    pub position_100ns: i64,
    pub seek_enabled: bool,
}

#[derive(Clone, Debug)]
pub enum MediaCommand {
    Seek(i64),
}

pub fn spawn_worker() -> (
    Sender<MediaCommand>,
    Receiver<Result<Option<MediaSnapshot>, String>>,
) {
    let (command_tx, command_rx) = mpsc::channel();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    thread::spawn(move || {
        // The Windows Runtime APIs need a COM apartment on the worker thread.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        loop {
            while let Ok(command) = command_rx.try_recv() {
                let MediaCommand::Seek(position) = command;
                let _ = seek(position);
            }
            let _ = snapshot_tx.send(snapshot());
            thread::sleep(Duration::from_millis(750));
        }
    });
    (command_tx, snapshot_rx)
}

fn snapshot() -> Result<Option<MediaSnapshot>, String> {
    let manager = block_on(
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    if let Ok(current) = manager.GetCurrentSession()
        && let Ok(snapshot) = read_session(&current)
    {
        return Ok(Some(snapshot));
    }
    if let Ok(sessions) = manager.GetSessions()
        && let Ok(size) = sessions.Size()
    {
        for index in 0..size {
            if let Ok(session) = sessions.GetAt(index)
                && let Ok(snapshot) = read_session(&session)
            {
                return Ok(Some(snapshot));
            }
        }
    }
    Ok(None)
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
    let seek_enabled = session
        .GetPlaybackInfo()
        .ok()
        .and_then(|info| info.Controls().ok())
        .and_then(|controls| controls.IsPlaybackPositionEnabled().ok())
        .unwrap_or(false)
        && end_100ns > start_100ns;
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
    })
}

fn seek(position_100ns: i64) -> Result<(), String> {
    let manager = block_on(
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let session = manager
        .GetCurrentSession()
        .map_err(|error| error.to_string())?;
    let operation = session
        .TryChangePlaybackPositionAsync(position_100ns)
        .map_err(|error| error.to_string())?;
    let _ = block_on(operation).map_err(|error| error.to_string())?;
    Ok(())
}
