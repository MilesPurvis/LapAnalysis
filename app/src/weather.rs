//! Weather at the track during a session, from Open-Meteo (free, no API key). It's fetched
//! once while online and kept in the library index, so it still shows offline at the track.
//! Only the track's coordinates and the session's date are sent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const ARCHIVE: &str = "https://archive-api.open-meteo.com/v1/archive";
/// The archive lags about five days; recent days come from the forecast API's history.
const RECENT: &str = "https://api.open-meteo.com/v1/forecast";
const HOURLY: &str = "temperature_2m,relative_humidity_2m,precipitation,weather_code,cloud_cover,wind_speed_10m,wind_gusts_10m";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Weather {
    /// Hourly readings covering the session, in the track's local time.
    pub hours: Vec<Hour>,
    pub fetched_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hour {
    /// Local time, e.g. `2026-09-07T13:00`.
    pub time: String,
    pub temp_c: Option<f64>,
    pub humidity: Option<f64>,
    pub precip_mm: Option<f64>,
    /// WMO weather code (0 clear … 95+ thunderstorm).
    pub code: Option<u8>,
    pub cloud: Option<f64>,
    pub wind_kmh: Option<f64>,
    pub gust_kmh: Option<f64>,
}

#[derive(Deserialize)]
struct Response {
    hourly: Hourly,
}

#[derive(Deserialize)]
struct Hourly {
    time: Vec<String>,
    temperature_2m: Vec<Option<f64>>,
    relative_humidity_2m: Vec<Option<f64>>,
    precipitation: Vec<Option<f64>>,
    weather_code: Vec<Option<u8>>,
    cloud_cover: Vec<Option<f64>>,
    wind_speed_10m: Vec<Option<f64>>,
    wind_gusts_10m: Vec<Option<f64>>,
}

/// Weather for a session starting at `date` (`yyyy-mm-dd`) `time` (`hh:mm:ss`) local, lasting
/// `duration_ms`, at the given position.
pub fn fetch(lat: f64, lon: f64, date: &str, time: &str, duration_ms: Option<u64>) -> Result<Weather> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let mut last_err = None;
    for base in [ARCHIVE, RECENT] {
        let hourly = match get(&agent, base, lat, lon, date) {
            Ok(h) => h,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        let hours = session_hours(hourly, time, duration_ms);
        if !hours.is_empty() && hours.iter().all(|h| h.temp_c.is_some()) {
            let fetched_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            return Ok(Weather { hours, fetched_at });
        }
    }
    match last_err {
        Some(e) => Err(e).context("couldn't reach Open-Meteo (no internet?)"),
        None => bail!("Open-Meteo has no weather for {date} yet"),
    }
}

fn get(agent: &ureq::Agent, base: &str, lat: f64, lon: f64, date: &str) -> Result<Hourly> {
    let resp: Response = agent
        .get(base)
        .query("latitude", format!("{lat:.4}"))
        .query("longitude", format!("{lon:.4}"))
        .query("start_date", date)
        .query("end_date", date)
        .query("hourly", HOURLY)
        .query("timezone", "auto")
        .call()?
        .body_mut()
        .read_json()?;
    Ok(resp.hourly)
}

/// The hours from the one the session starts in through the one it ends in.
fn session_hours(h: Hourly, start: &str, duration_ms: Option<u64>) -> Vec<Hour> {
    let hour = |t: &str| t.get(..2).and_then(|s| s.parse::<u64>().ok());
    let Some(first) = hour(start) else { return vec![] };
    let minutes: u64 = start.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
    let last = first + (minutes + duration_ms.unwrap_or(0) / 60_000) / 60;
    (0..h.time.len())
        .filter(|&i| h.time[i].get(11..).and_then(hour).is_some_and(|x| (first..=last).contains(&x)))
        .map(|i| Hour {
            time: h.time[i].clone(),
            temp_c: h.temperature_2m[i],
            humidity: h.relative_humidity_2m[i],
            precip_mm: h.precipitation[i],
            code: h.weather_code[i],
            cloud: h.cloud_cover[i],
            wind_kmh: h.wind_speed_10m[i],
            gust_kmh: h.wind_gusts_10m[i],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_hours_a_session_covers() {
        let times: Vec<String> = (0..24).map(|h| format!("2026-09-07T{h:02}:00")).collect();
        let n = times.len();
        let h = Hourly {
            time: times,
            temperature_2m: vec![Some(20.0); n],
            relative_humidity_2m: vec![None; n],
            precipitation: vec![None; n],
            weather_code: vec![None; n],
            cloud_cover: vec![None; n],
            wind_speed_10m: vec![None; n],
            wind_gusts_10m: vec![None; n],
        };
        // 13:43 for 18 minutes runs into the 14:00 hour.
        let hours = session_hours(h, "13:43:51", Some(1_074_000));
        assert_eq!(hours.iter().map(|h| &h.time[11..]).collect::<Vec<_>>(), ["13:00", "14:00"]);
    }
}
