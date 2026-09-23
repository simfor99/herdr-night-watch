#[cfg(windows)]
use winreg::RegKey;
#[cfg(windows)]
use winreg::enums::HKEY_CURRENT_USER;

#[allow(dead_code)]
const KEY: &str = r"Software\HerdrNachtwaechter";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    German,
    English,
}

impl Language {
    #[allow(dead_code)]
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            let value = RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey(KEY)
                .and_then(|key| key.get_value::<String, _>("Language"));
            if value.is_ok_and(|value| value.eq_ignore_ascii_case("en")) {
                Self::English
            } else {
                Self::German
            }
        }
        #[cfg(not(windows))]
        {
            Self::German
        }
    }
    #[allow(dead_code)]
    pub fn set(self) -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            let value = match self {
                Self::German => "de",
                Self::English => "en",
            };
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
            key.set_value("Language", &value)?;
        }
        Ok(())
    }
    pub fn text(self, german: &'static str, english: &'static str) -> &'static str {
        match self {
            Self::German => german,
            Self::English => english,
        }
    }
}
