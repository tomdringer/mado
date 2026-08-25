// ─── Clock helpers ────────────────────────────────────────────────────────────
//
// Time is formatted from the system clock via chrono::Local.
// Weather is fetched from wttr.in using the system `curl` binary (no extra
// network crate required). The JSON response is parsed with serde_json.

use chrono::Local;
use serde::Deserialize;

// ── Time / date ───────────────────────────────────────────────────────────────

pub fn current_time() -> String {
    Local::now().format("%H:%M").to_string()
}

pub fn current_date() -> String {
    // e.g. "Monday, 22 August"
    Local::now().format("%A, %-d %B").to_string()
}

// ── Weather ───────────────────────────────────────────────────────────────────

pub struct WeatherData {
    pub icon: &'static str,
    pub temp: String,   // e.g. "+15°C"
    pub desc: String,   // e.g. "Partly cloudy"
    pub loc:  String,   // e.g. "Melbourne"
}

#[derive(Deserialize)]
struct WttrRoot {
    current_condition: Vec<Condition>,
    nearest_area:      Vec<Area>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct Condition {
    temp_C:       String,
    weatherCode:  String,
    weatherDesc:  Vec<Sv>,
}

#[derive(Deserialize)]
struct Area {
    #[serde(rename = "areaName")]
    area_name: Vec<Sv>,
}

#[derive(Deserialize)]
struct Sv { value: String }

fn icon_for_code(code: &str) -> &'static str {
    match code.parse::<u16>().unwrap_or(0) {
        113                                  => "☀",
        116                                  => "⛅",
        119 | 122                            => "☁",
        143 | 248 | 260                      => "🌫",
        176 | 263 | 266 | 293..=308          => "🌧",
        179 | 227 | 230 | 323..=338 | 371 | 395 => "❄",
        182 | 185 | 281 | 284 | 311..=320    => "🌨",
        200 | 386..=395                      => "⛈",
        _                                    => "🌡",
    }
}

/// Fetch current weather from wttr.in via the system `curl` binary.
/// Returns `None` if the request or parse fails — the caller retries later.
pub fn fetch_weather() -> Option<WeatherData> {
    let out = std::process::Command::new("curl")
        .args(["-s", "--max-time", "10", "https://wttr.in/?format=j1"])
        .output()
        .ok()?;

    if !out.status.success() { return None; }

    let text = String::from_utf8(out.stdout).ok()?;
    let root: WttrRoot = serde_json::from_str(&text).ok()?;

    let cond = root.current_condition.into_iter().next()?;
    let area = root.nearest_area.into_iter().next()?;

    Some(WeatherData {
        icon: icon_for_code(&cond.weatherCode),
        temp: format!("{}°C", cond.temp_C),
        desc: cond.weatherDesc.into_iter().next().map(|s| s.value).unwrap_or_default(),
        loc:  area.area_name.into_iter().next().map(|s| s.value).unwrap_or_default(),
    })
}
