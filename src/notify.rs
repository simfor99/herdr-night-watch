use crate::{backend::CompletionAction, language::Language};
use std::iter;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDOK, MB_ICONINFORMATION, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeAction {
    Confirm,
    Cancel,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(iter::once(0)).collect()
}

pub fn completion_notice(
    language: Language,
    demo: bool,
    warning_seconds: u64,
    completion_action: CompletionAction,
    network_triggered: bool,
) -> NoticeAction {
    let (title, text) = if demo {
        (
            language.text("Herdr-Nachtwächter - Demo", "Herdr Night Watch - Demo"),
            format!(
                "{}\n\n{} {warning_seconds} {}.",
                language.text(
                    "Alle simulierten Agenten sind fertig.",
                    "All simulated agents are finished."
                ),
                language.text(
                    "In einem echten Nachtlauf würde Windows jetzt in etwa",
                    "In a real night run, Windows would shut down in about"
                ),
                language.text(
                    "Sekunden herunterfahren. Diese Demo fährt Windows nicht herunter",
                    "seconds. This demo does not shut down Windows"
                )
            ),
        )
    } else {
        let (action, abort) = match completion_action {
            CompletionAction::Sleep => (
                format!(
                    "{} {warning_seconds} {}.",
                    language.text(
                        "Windows wechselt in etwa",
                        "Windows will enter sleep in about"
                    ),
                    language.text("Sekunden in den Energiesparmodus", "seconds")
                ),
                language.text(
                    "Klicke auf Abbrechen, um den Energiesparmodus zu verhindern.",
                    "Click Cancel to prevent sleep.",
                ),
            ),
            CompletionAction::Shutdown => (
                format!(
                    "{} {warning_seconds} {}.",
                    language.text("Windows fährt in etwa", "Windows will shut down in about"),
                    language.text("Sekunden herunter", "seconds")
                ),
                language.text(
                    "Klicke auf Abbrechen, um den Shutdown zu verhindern.",
                    "Click Cancel to prevent shutdown.",
                ),
            ),
        };
        let network_note = if network_triggered {
            language.text(
                "Seit fünf Minuten besteht keine Internetverbindung.\n\n",
                "There has been no internet connection for five minutes.\n\n",
            )
        } else {
            ""
        };
        (language.text("Herdr-Nachtwächter", "Herdr Night Watch"), format!("{network_note}{}\n\n{action}\n\n{}\n{abort}", language.text("Alle überwachten Herdr-Agenten sind fertig oder können ohne Internet nicht weiterarbeiten.", "All monitored Herdr agents are finished or cannot continue without internet."), language.text("Klicke auf OK, um die Aktion sofort auszuführen.", "Click OK to execute the action immediately.")))
    };
    let title = wide(title);
    let text = wide(&text);
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    notice_action_from_result(result)
}
fn notice_action_from_result(result: i32) -> NoticeAction {
    if result == IDOK {
        NoticeAction::Confirm
    } else {
        NoticeAction::Cancel
    }
}

#[cfg(test)]
mod tests {
    use super::{NoticeAction, notice_action_from_result};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IDCANCEL, IDOK};
    #[test]
    fn only_explicit_ok_confirms() {
        assert_eq!(notice_action_from_result(IDOK), NoticeAction::Confirm);
        assert_eq!(notice_action_from_result(IDCANCEL), NoticeAction::Cancel);
        assert_eq!(notice_action_from_result(0), NoticeAction::Cancel);
    }
}
