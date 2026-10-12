use crate::{
    backend::{self, CompletionAction},
    language::Language,
};
use std::iter;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GetClassNameW, GetWindowTextW, IDOK, MB_ICONINFORMATION, MB_OKCANCEL,
    MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW, PostMessageW, WM_CLOSE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeAction {
    Confirm,
    Cancel,
    Dismissed,
}

struct NoticeWindow {
    title: Vec<u16>,
    posted: bool,
}

unsafe extern "system" fn close_owned_notice(hwnd: HWND, parameter: LPARAM) -> BOOL {
    let request = unsafe { &mut *(parameter as *mut NoticeWindow) };
    let mut class = [0_u16; 32];
    let class_len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    if String::from_utf16_lossy(&class[..class_len as usize]) != "#32770" {
        return 1;
    }
    let mut title = vec![0_u16; request.title.len() + 1];
    let title_len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    if title[..title_len as usize] == request.title[..request.title.len() - 1] {
        request.posted = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } != 0;
        return 0;
    }
    1
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(iter::once(0)).collect()
}

fn forced_shutdown_warning(language: Language, action: CompletionAction) -> &'static str {
    if action == CompletionAction::Shutdown {
        language.text(
            "Offene Apps werden ohne Rückfrage geschlossen. Speichere deine Arbeit oder klicke auf Abbrechen.",
            "Open apps will be closed without prompting. Save your work or click Cancel.",
        )
    } else {
        ""
    }
}

pub fn completion_notice(
    language: Language,
    demo: bool,
    warning_seconds: u64,
    completion_action: CompletionAction,
    network_triggered: bool,
    warning_id: Option<&str>,
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
        let forced_warning = forced_shutdown_warning(language, completion_action);
        let warning_break = if forced_warning.is_empty() {
            ""
        } else {
            "\n\n"
        };
        (
            language.text("Herdr-Nachtwächter", "Herdr Night Watch"),
            format!(
                "{network_note}{}\n\n{action}\n\n{}\n\n{forced_warning}{warning_break}{abort}",
                language.text(
                    "Alle überwachten Herdr-Agenten sind fertig oder können ohne Internet nicht weiterarbeiten.",
                    "All monitored Herdr agents are finished or cannot continue without internet.",
                ),
                language.text(
                    "Klicke auf OK, um die Aktion sofort auszuführen.",
                    "Click OK to execute the action immediately.",
                ),
            ),
        )
    };
    let title = wide(title);
    let text = wide(&text);
    let invalidated = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = mpsc::channel();
    let monitor = warning_id.map(|warning_id| {
        let warning_id = warning_id.to_owned();
        let dialog_thread = unsafe { GetCurrentThreadId() };
        let title = title.clone();
        let invalidated = Arc::clone(&invalidated);
        thread::spawn(move || {
            let mut stale = false;
            loop {
                match stop_rx.recv_timeout(Duration::from_millis(if stale { 100 } else { 1000 })) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                // Read errors retain the warning; they never authorize completion.
                if !stale {
                    stale = matches!(backend::warning_is_current(&warning_id), Ok(false));
                }
                if stale {
                    invalidated.store(true, Ordering::SeqCst);
                    let mut request = NoticeWindow {
                        title: title.clone(),
                        posted: false,
                    };
                    unsafe {
                        EnumThreadWindows(
                            dialog_thread,
                            Some(close_owned_notice),
                            &mut request as *mut _ as LPARAM,
                        );
                    }
                    if request.posted {
                        break;
                    }
                }
            }
        })
    });
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    let _ = stop_tx.send(());
    if let Some(monitor) = monitor {
        let _ = monitor.join();
    }
    notice_action(result, invalidated.load(Ordering::SeqCst))
}

fn notice_action(result: i32, withdrawn: bool) -> NoticeAction {
    if withdrawn {
        NoticeAction::Dismissed
    } else {
        notice_action_from_result(result)
    }
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
    use super::{NoticeAction, forced_shutdown_warning, notice_action, notice_action_from_result};
    use crate::{backend::CompletionAction, language::Language};
    use windows_sys::Win32::UI::WindowsAndMessaging::{IDCANCEL, IDOK};
    #[test]
    fn only_explicit_ok_confirms() {
        assert_eq!(notice_action_from_result(IDOK), NoticeAction::Confirm);
        assert_eq!(notice_action_from_result(IDCANCEL), NoticeAction::Cancel);
        assert_eq!(notice_action_from_result(0), NoticeAction::Cancel);
    }

    #[test]
    fn withdrawn_warning_never_confirms_or_cancels_a_run() {
        assert_eq!(notice_action(IDOK, true), NoticeAction::Dismissed);
        assert_eq!(notice_action(IDCANCEL, true), NoticeAction::Dismissed);
        assert_eq!(notice_action(0, true), NoticeAction::Dismissed);
    }

    #[test]
    fn withdrawal_closes_only_its_own_dialog() {
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, EnumThreadWindows, MSG, PM_REMOVE, PeekMessageW,
            WM_CLOSE,
        };
        let class = super::wide("#32770");
        let title = super::wide("Herdr-owned-test-warning");
        let other_title = super::wide("Unrelated-test-dialog");
        unsafe {
            // Invisible windows owned by this test thread; no user window is touched.
            let own = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                20,
                20,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            let other = CreateWindowExW(
                0,
                class.as_ptr(),
                other_title.as_ptr(),
                0,
                0,
                0,
                20,
                20,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!own.is_null() && !other.is_null());
            let mut request = super::NoticeWindow {
                title,
                posted: false,
            };
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(super::close_owned_notice),
                &mut request as *mut _ as isize,
            );
            let mut message: MSG = std::mem::zeroed();
            let own_closed = PeekMessageW(&mut message, own, WM_CLOSE, WM_CLOSE, PM_REMOVE) != 0;
            let other_closed =
                PeekMessageW(&mut message, other, WM_CLOSE, WM_CLOSE, PM_REMOVE) != 0;
            DestroyWindow(own);
            DestroyWindow(other);
            assert!(request.posted && own_closed);
            assert!(!other_closed);
        }
    }

    #[test]
    fn forced_shutdown_warning_explains_that_apps_close_without_prompt() {
        assert!(
            forced_shutdown_warning(Language::German, CompletionAction::Shutdown)
                .contains("ohne Rückfrage")
        );
        assert!(
            forced_shutdown_warning(Language::English, CompletionAction::Shutdown)
                .contains("without prompting")
        );
        assert!(forced_shutdown_warning(Language::German, CompletionAction::Sleep).is_empty());
    }
}
