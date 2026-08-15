use anyhow::{Result, anyhow};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

/// Keep Windows from entering automatic system sleep while Night Watch is active.
/// An explicit sleep or shutdown request is not blocked by this execution state.
pub fn set_prevent_sleep(active: bool) -> Result<()> {
    let flags = if active {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    let result = unsafe { SetThreadExecutionState(flags) };
    if result == 0 {
        Err(anyhow!(
            "Windows-Energiesperre konnte nicht aktualisiert werden"
        ))
    } else {
        Ok(())
    }
}
