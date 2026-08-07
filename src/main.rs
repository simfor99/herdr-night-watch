#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod configuration;
#[cfg(windows)]
mod language;
#[cfg(windows)]
mod live_status;
#[cfg(windows)]
mod notify;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod system_metrics;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod window_settings;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|argument| argument == "--settings") {
        settings::run()
    } else if std::env::args().any(|argument| argument == "--live-status") {
        live_status::run()
    } else {
        tray::run()
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Herdr Night Watch is a Windows tray application.");
}
