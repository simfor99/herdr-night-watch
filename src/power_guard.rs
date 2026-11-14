use anyhow::{Result, anyhow};
use std::{marker::PhantomData, rc::Rc};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Power::{
            ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, PowerClearRequest,
            PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
            SetThreadExecutionState,
        },
        Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};

const POWER_REQUEST_REASON: &str = "Herdr Nachtwaechter verhindert automatischen Standby";

#[must_use = "the guard must live as long as the tray app"]
pub struct TrayPowerGuard {
    request_handle: Option<HANDLE>,
    // SetThreadExecutionState belongs to the calling thread. Keep Drop and
    // refresh on the same tray thread that acquired the request.
    _thread_bound: PhantomData<Rc<()>>,
}

impl TrayPowerGuard {
    /// Keep Windows from entering automatic system sleep for the tray lifetime.
    /// An explicit sleep or shutdown request is not blocked by this execution state.
    pub fn acquire() -> Result<Self> {
        let request_handle = create_system_power_request().ok();
        let exec_result = set_tray_running(true);
        if request_handle.is_none() {
            exec_result?;
        }
        Ok(Self {
            request_handle,
            _thread_bound: PhantomData,
        })
    }

    /// Re-apply the thread execution state (e.g. periodically or after resume from sleep)
    /// so Windows does not fall back to System Idle standby if the thread flag was cleared.
    pub fn refresh(&self) -> Result<()> {
        let exec_result = set_tray_running(true);
        if let Some(handle) = self.request_handle {
            let _ = unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) };
            Ok(())
        } else {
            exec_result
        }
    }
}

impl Drop for TrayPowerGuard {
    fn drop(&mut self) {
        let _ = set_tray_running(false);
        if let Some(handle) = self.request_handle.take() {
            unsafe {
                let _ = PowerClearRequest(handle, PowerRequestSystemRequired);
                let _ = CloseHandle(handle);
            }
        }
    }
}

fn create_system_power_request() -> Result<HANDLE> {
    let mut reason_utf16: Vec<u16> = POWER_REQUEST_REASON
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let context = REASON_CONTEXT {
        Version: 0,
        Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
        Reason: REASON_CONTEXT_0 {
            SimpleReasonString: reason_utf16.as_mut_ptr(),
        },
    };
    let handle = unsafe { PowerCreateRequest(&context) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(anyhow!("PowerCreateRequest fehlgeschlagen"));
    }
    let set_ok = unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) };
    if set_ok == 0 {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(anyhow!("PowerSetRequest fehlgeschlagen"));
    }
    Ok(handle)
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
    fn tray_guard_can_be_acquired_refreshed_and_released_on_windows() {
        let guard = TrayPowerGuard::acquire().expect("power guard should be available on Windows");
        guard
            .refresh()
            .expect("power guard refresh should succeed on Windows");
        drop(guard);
    }
}
