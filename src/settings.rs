use crate::configuration::{self, Configuration};
use anyhow::Result;
use eframe::egui;
use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn open() -> Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--settings")
        .spawn()?;
    Ok(())
}
pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 270.0])
            .with_title("Herdr Night Watch - Setup"),
        ..Default::default()
    };
    eframe::run_native(
        "Herdr Night Watch - Setup",
        options,
        Box::new(|_| Ok(Box::new(SettingsApp::new()))),
    )
    .map_err(|error| anyhow::anyhow!("Einrichtung konnte nicht ausgeführt werden: {error}"))
}
struct SettingsApp {
    configuration: Configuration,
    message: Option<String>,
}
impl SettingsApp {
    fn new() -> Self {
        Self {
            configuration: configuration::load(),
            message: None,
        }
    }
}
impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        ui.heading("Herdr Night Watch setup");
        ui.label("Works with Codex, Claude Code, or other agents - Herdr is the only runtime requirement.");
        ui.add_space(12.0);
        ui.label("WSL distribution");
        ui.text_edit_singleline(&mut self.configuration.distro);
        ui.add_space(8.0);
        ui.label("Path to herdr-night-watch.py inside WSL");
        ui.text_edit_singleline(&mut self.configuration.watcher_path);
        ui.small(
            "Example: /home/your-name/projects/herdr-night-watch/watcher/herdr-night-watch.py",
        );
        ui.add_space(14.0);
        if ui.button("Save configuration").clicked() {
            self.message = Some(match configuration::save(&self.configuration) {
                Ok(()) => "Saved. The tray app uses the new configuration immediately.".into(),
                Err(error) => format!("Could not save: {error}"),
            });
        }
        if let Some(message) = &self.message {
            ui.add_space(8.0);
            ui.label(message);
        }
    }
}
