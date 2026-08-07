use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const KEY: &str = r"Software\HerdrNachtwaechter";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    German,
    English,
}

impl Language {
    pub fn current() -> Self {
        let value = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(KEY)
            .and_then(|key| key.get_value::<String, _>("Language"));
        if value.is_ok_and(|value| value.eq_ignore_ascii_case("en")) {
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
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
        key.set_value("Language", &value)?;
        Ok(())
    }
    pub fn text(self, german: &'static str, english: &'static str) -> &'static str {
        match self {
            Self::German => german,
            Self::English => english,
        }
    }
}
