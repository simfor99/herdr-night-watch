use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const KEY: &str = r"HKCU\Software\HerdrNachtwaechter";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    German,
    English,
}

impl Language {
    pub fn current() -> Self {
        let output = Command::new("reg.exe")
            .args(["query", KEY, "/v", "Language"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if output.ok().is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line.trim().to_ascii_lowercase().ends_with(" en"))
        }) {
            Self::English
        } else {
            Self::German
        }
    }
    pub fn set(self) -> anyhow::Result<()> {
        let value = match self {
            Self::German => "de",
            Self::English => "en",
        };
        let status = Command::new("reg.exe")
            .args([
                "add", KEY, "/v", "Language", "/t", "REG_SZ", "/d", value, "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        anyhow::ensure!(status.success(), "language setting could not be saved");
        Ok(())
    }
    pub fn text(self, german: &'static str, english: &'static str) -> &'static str {
        match self {
            Self::German => german,
            Self::English => english,
        }
    }
}
