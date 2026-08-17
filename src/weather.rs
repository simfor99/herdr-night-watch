use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const REGISTRY_KEY: &str = r"Software\HerdrNachtwaechter";
const LOCATION_VALUE: &str = "WeatherLocation";
const GEOCODING_ENDPOINT: &str = "https://geocoding-api.open-meteo.com/v1/search";
const FORECAST_ENDPOINT: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WeatherLocation {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub admin1: String,
    #[serde(default)]
    pub timezone: String,
}

#[derive(Clone, Debug)]
pub struct WeatherReading {
    pub temperature_c: f64,
    pub observed_at: String,
    pub location: WeatherLocation,
    pub moon_phase: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct GeocodingResponse {
    results: Option<Vec<WeatherLocation>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ForecastResponse {
    current: Option<CurrentWeather>,
    daily: Option<DailyWeather>,
}

#[derive(Clone, Debug, Deserialize)]
struct CurrentWeather {
    temperature_2m: f64,
    time: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DailyWeather {
    moon_phase: Option<Vec<f64>>,
}

pub fn current_location() -> WeatherLocation {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(REGISTRY_KEY)
        .and_then(|key| key.get_value::<String, _>(LOCATION_VALUE))
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_else(default_location)
}

pub fn save_location(location: &WeatherLocation) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(REGISTRY_KEY)?;
    let value =
        serde_json::to_string(location).context("Wetterort konnte nicht gespeichert werden")?;
    key.set_value(LOCATION_VALUE, &value)
        .context("Wetterort konnte nicht gespeichert werden")?;
    Ok(())
}

pub fn search_locations(
    query: &str,
    language: crate::language::Language,
) -> Result<Vec<WeatherLocation>> {
    let query = query.trim();
    if query.len() < 2 {
        return Ok(Vec::new());
    }
    let language = match language {
        crate::language::Language::German => "de",
        crate::language::Language::English => "en",
    };
    let url = format!(
        "{GEOCODING_ENDPOINT}?name={}&count=8&language={language}&format=json",
        percent_encode(query)
    );
    let payload: GeocodingResponse = fetch_json(&url)?;
    Ok(payload.results.unwrap_or_default())
}

pub fn fetch_current(location: WeatherLocation) -> Result<WeatherReading> {
    let url = format!(
        "{FORECAST_ENDPOINT}?latitude={:.5}&longitude={:.5}&current=temperature_2m&daily=moon_phase&temperature_unit=celsius&timezone=auto",
        location.latitude, location.longitude
    );
    let payload: ForecastResponse = fetch_json(&url)?;
    let current = payload
        .current
        .context("Der Wetterdienst hat keine aktuelle Temperatur geliefert")?;
    Ok(WeatherReading {
        temperature_c: current.temperature_2m,
        observed_at: current.time,
        location,
        moon_phase: payload
            .daily
            .and_then(|daily| daily.moon_phase)
            .and_then(|phases| phases.first().copied())
            .filter(|phase| phase.is_finite())
            .unwrap_or_else(estimated_moon_phase),
    })
}

/// Returns the current lunar phase as a fraction in [0, 1): new moon at 0,
/// full moon at 0.5. This keeps the UI useful while the weather request is
/// still loading or temporarily unavailable.
pub fn estimated_moon_phase() -> f64 {
    const KNOWN_NEW_MOON_UNIX: f64 = 947_182_440.0; // 2000-01-06 18:14 UTC
    const SYNODIC_MONTH_SECONDS: f64 = 29.530_588_853 * 86_400.0;
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(KNOWN_NEW_MOON_UNIX);
    ((unix_seconds - KNOWN_NEW_MOON_UNIX) / SYNODIC_MONTH_SECONDS).rem_euclid(1.0)
}

fn fetch_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T> {
    let script = "$ProgressPreference='SilentlyContinue'; $ErrorActionPreference='Stop'; "
        .to_owned()
        + "(Invoke-RestMethod -UseBasicParsing -TimeoutSec 10 -Uri '"
        + &url.replace('\'', "''")
        + "') | ConvertTo-Json -Depth 5 -Compress";
    let output = Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("Wetterdienst konnte nicht gestartet werden")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(anyhow!(if detail.is_empty() {
            "Wetterdienst ist momentan nicht erreichbar".to_owned()
        } else {
            format!("Wetterdienst: {detail}")
        }));
    }
    let json = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(json.trim())
        .context("Antwort des Wetterdienstes konnte nicht gelesen werden")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn default_location() -> WeatherLocation {
    WeatherLocation {
        name: "Leipzig".into(),
        latitude: 51.33962,
        longitude: 12.37129,
        country: "Deutschland".into(),
        country_code: "DE".into(),
        admin1: "Sachsen".into(),
        timezone: "Europe/Berlin".into(),
    }
}
