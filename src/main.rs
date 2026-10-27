#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(any(windows, test))]
mod quota;
#[cfg(any(windows, test))]
mod status_channel;

#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod backend;
#[cfg(windows)]
mod configuration;
#[cfg(any(windows, test))]
mod language;
#[cfg(windows)]
mod live_status;
#[cfg(windows)]
mod log_viewer;
#[cfg(windows)]
mod media;
#[cfg(windows)]
mod notify;
#[cfg(windows)]
mod power_guard;
#[cfg(any(windows, test))]
mod quota_layout;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod system_metrics;
#[cfg(windows)]
mod taskbar;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod tray_history;
#[cfg(windows)]
mod weather;
#[cfg(windows)]
mod weather_location;
#[cfg(windows)]
mod window_chrome;
#[cfg(windows)]
mod window_settings;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let result = if std::env::args().any(|argument| argument == "--settings") {
        settings::run()
    } else if std::env::args().any(|argument| argument == "--live-status") {
        live_status::run()
    } else if std::env::args().any(|argument| argument == "--completion-log") {
        log_viewer::run()
    } else if std::env::args().any(|argument| argument == "--weather-location") {
        weather_location::run()
    } else {
        tray::run()
    };
    backend::shutdown_status_channel();
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Herdr Night Watch is a Windows tray application.");
}
