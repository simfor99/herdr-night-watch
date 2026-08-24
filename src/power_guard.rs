use anyhow::{Result, anyhow};
use std::{marker::PhantomData, rc::Rc};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState,
};

#[must_use = "the guard must live as long as the tray app"]
pub struct TrayPowerGuard {
    // SetThreadExecutionState belongs to the calling thread. Keep Drop on the
    // same tray thread that acquired the request.
    _thread_bound: PhantomData<Rc<()>>,
}

impl TrayPowerGuard {
    /// Keep Windows from entering automatic system sleep for the tray lifetime.
    /// An explicit sleep or shutdown request is not blocked by this execution state.
    pub fn acquire() -> Result<Self> {
        set_tray_running(true)?;
        Ok(Self {
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for TrayPowerGuard {
    fn drop(&mut self) {
        let _ = set_tray_running(false);
    }
}

fn execution_state_flags(tray_running: bool) -> EXECUTION_STATE {
    if tray_running {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    }
}

fn set_tray_running(tray_running: bool) -> Result<()> {
    let result = unsafe { SetThreadExecutionState(execution_state_flags(tray_running)) };
    if result == 0 {
        Err(anyhow!(
            "Windows-Energiesperre konnte nicht aktualisiert werden"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{TrayPowerGuard, execution_state_flags};
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    #[test]
    fn running_tray_keeps_the_system_awake_without_forcing_the_display_on() {
        let flags = execution_state_flags(true);

        assert_eq!(flags, ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        assert_eq!(flags & ES_DISPLAY_REQUIRED, 0);
    }

    #[test]
    fn stopped_tray_releases_the_continuous_system_requirement() {
        assert_eq!(execution_state_flags(false), ES_CONTINUOUS);
    }

    #[test]
    fn tray_guard_can_be_acquired_and_released_on_windows() {
        let guard = TrayPowerGuard::acquire().expect("power guard should be available on Windows");
        drop(guard);
    }
}
