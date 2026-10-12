//! Lap comparison. Laps are lined up by distance travelled, so the same corner sits at the
//! same x across laps, and each lap's time gained or lost against a reference lap is worked
//! out along the way.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::library::{lap_window, laps, value, Lap, Library, LibraryItem, Loaded};

/// A reference lap plus up to four others.
pub const MAX_LAPS: usize = 5;
/// Channel name for GPS speed in km/h; every other channel is matched by its logger name.
pub const GPS_SPEED: &str = "GPS Speed";

#[derive(Serialize)]
pub struct Comparison {
    /// Distance in metres along the reference lap; every lap is sampled at these points.
    pub distance: Vec<f64>,
    pub laps: Vec<ComparedLap>,
    /// Units of each requested channel (from the first lap that has it).
    pub units: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct ComparedLap {
    #[serde(flatten)]
    pub session: LibraryItem,
    pub lap: i32,
    pub lap_ms: i64,
    /// Distance the lap covered by GPS, before scaling to the reference lap's length.
    pub length_m: f64,
    /// Seconds behind (+) or ahead (−) of the reference at each distance.
    pub delta_s: Vec<f64>,
    /// Requested channels, by name; `null` where the session doesn't have the channel.
    pub channels: BTreeMap<String, Vec<Option<f64>>>,
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
}

/// A session's laps, for picking what to compare.
#[derive(Serialize)]
pub struct SessionLaps {
    #[serde(flatten)]
    pub session: LibraryItem,
    pub laps: Vec<Lap>,
    /// First GPS fix, used to offer laps from the same track.
    pub position: Option<(f64, f64)>,
    pub channels: Vec<String>,
}

/// Every library session that has GPS, with its laps.
pub fn catalog(lib: &Library) -> Result<Vec<SessionLaps>> {
    let mut out = vec![];
    for session in lib.list()? {
        let loaded = lib.load(&session.name)?;
        let Some(x) = loaded.xrk.as_ref() else { continue };
        let Some(g) = x.gps.as_ref().filter(|g| !g.timecodes.is_empty()) else { continue };
        let position = g.latitude.iter().zip(&g.longitude).find(|(la, lo)| **la != 0.0 || **lo != 0.0).map(|(la, lo)| (*la, *lo));
        let channels = std::iter::once(GPS_SPEED.to_string()).chain(x.channels.iter().map(|c| c.name.clone())).collect();
        out.push(SessionLaps { laps: laps(x), position, channels, session });
    }
    Ok(out)
}

/// Compare laps given as `(file, lap number)`; the first one is the reference.
pub fn compare(lib: &Library, wanted: &[(String, i32)], channels: &[String], points: usize) -> Result<Comparison> {
    if wanted.is_empty() || wanted.len() > MAX_LAPS {
        bail!("pick between 1 and {MAX_LAPS} laps");
    }
    let laps = wanted.iter().map(|(file, lap)| PreparedLap::new(lib, file, *lap)).collect::<Result<Vec<_>>>()?;
    let ref_len = laps[0].trace.length();
    if ref_len < 100.0 {
        bail!("the reference lap has no usable GPS distance");
    }
    let points = points.clamp(50, 5000);
    let distance: Vec<f64> = (0..points).map(|i| ref_len * i as f64 / (points - 1) as f64).collect();
    let ref_times: Vec<f64> = distance.iter().map(|&d| laps[0].trace.time_at(d, ref_len)).collect();

    let units = channels
        .iter()
        .filter_map(|name| laps.iter().find_map(|l| l.units(name)).map(|u| (name.clone(), u)))
        .collect();
    let mut out = vec![];
    for lap in laps {
        let times: Vec<f64> = distance.iter().map(|&d| lap.trace.time_at(d, ref_len)).collect();
        let channels = channels.iter().map(|name| (name.clone(), times.iter().map(|&t| lap.sample(name, t)).collect())).collect();
        out.push(ComparedLap {
            delta_s: times.iter().zip(&ref_times).map(|(t, r)| t - r).collect(),
            lat: times.iter().map(|&t| interp(&lap.gps_t, &lap.lat, t)).collect(),
            lon: times.iter().map(|&t| interp(&lap.gps_t, &lap.lon, t)).collect(),
            length_m: lap.trace.length(),
            lap: lap.lap,
            lap_ms: lap.lap_ms,
            channels,
            session: lap.session,
        });
    }
    Ok(Comparison { distance, laps: out, units })
}

/// Elapsed time and distance covered at each GPS fix of a lap.
pub(crate) struct Trace {
    t: Vec<f64>,
    d: Vec<f64>,
}

impl Trace {
    /// Integrate speed (m/s) over time (s), trapezoid rule.
    fn from_speed(t: &[f64], speed: &[f64]) -> Self {
        let mut d = Vec::with_capacity(t.len());
        let mut covered = 0.0;
        for k in 0..t.len() {
            if k > 0 {
                covered += (t[k] - t[k - 1]) * (speed[k] + speed[k - 1]) / 2.0;
            }
            d.push(covered);
        }
        Self { t: t.to_vec(), d }
    }

    pub(crate) fn length(&self) -> f64 {
        self.d.last().copied().unwrap_or(0.0)
    }

    /// When the lap reached a point `d` metres along a lap `ref_len` long. Laps are scaled to
    /// the reference length so a slightly different line doesn't drift the comparison.
    pub(crate) fn time_at(&self, d: f64, ref_len: f64) -> f64 {
        interp(&self.d, &self.t, d * self.length() / ref_len)
    }
}

pub(crate) struct PreparedLap {
    pub(crate) session: LibraryItem,
    pub(crate) lap: i32,
    pub(crate) lap_ms: i64,
    start_ms: i64,
    loaded: Arc<Loaded>,
    /// GPS fixes in seconds from the lap start, with a fix pinned at both ends of the lap.
    pub(crate) gps_t: Vec<f64>,
    speed_kmh: Vec<f64>,
    pub(crate) lat: Vec<f64>,
    pub(crate) lon: Vec<f64>,
    pub(crate) trace: Trace,
}

impl PreparedLap {
    pub(crate) fn new(lib: &Library, file: &str, lap: i32) -> Result<Self> {
        let loaded = lib.load(file)?;
        let x = loaded.xrk.as_ref().with_context(|| format!("{file} isn't a session"))?;
        let g = x.gps.as_ref().with_context(|| format!("{file} has no GPS"))?;
        let (t0, t1) = lap_window(x, Some(lap))?;
        let (mut gps_t, mut speed, mut lat, mut lon) = (vec![], vec![], vec![], vec![]);
        for k in 0..g.timecodes.len() {
            if (t0..=t1).contains(&g.timecodes[k]) {
                gps_t.push((g.timecodes[k] - t0) as f64 / 1000.0);
                speed.push(g.speed[k]);
                lat.push(g.latitude[k]);
                lon.push(g.longitude[k]);
            }
        }
        if gps_t.len() < 2 {
            bail!("{file} lap {lap} has no GPS fixes");
        }
        // Pin both ends to the lap's start and finish so the delta at the line is the exact
        // lap-time gap, not off by a fraction of the GPS interval.
        let end = (t1 - t0) as f64 / 1000.0;
        if gps_t[0] > 0.0 {
            gps_t.insert(0, 0.0);
            speed.insert(0, speed[0]);
            lat.insert(0, lat[0]);
            lon.insert(0, lon[0]);
        }
        if gps_t[gps_t.len() - 1] < end {
            gps_t.push(end);
            speed.push(speed[speed.len() - 1]);
            lat.push(lat[lat.len() - 1]);
            lon.push(lon[lon.len() - 1]);
        }
        let trace = Trace::from_speed(&gps_t, &speed);
        Ok(Self {
            session: lib.item(file)?,
            lap,
            lap_ms: t1 - t0,
            start_ms: t0,
            speed_kmh: speed.iter().map(|v| v * 3.6).collect(),
            loaded: loaded.clone(),
            gps_t,
            lat,
            lon,
            trace,
        })
    }

    fn units(&self, name: &str) -> Option<String> {
        if name == GPS_SPEED {
            return Some("km/h".into());
        }
        self.loaded.xrk.as_ref()?.channels.iter().find(|c| c.name == name).map(|c| c.units.clone())
    }

    /// A channel's value `t` seconds into the lap.
    pub(crate) fn sample(&self, name: &str, t: f64) -> Option<f64> {
        if name == GPS_SPEED {
            return Some(interp(&self.gps_t, &self.speed_kmh, t));
        }
        let x = self.loaded.xrk.as_ref()?;
        let c = x.channels.iter().find(|c| c.name == name)?;
        let at = self.start_ms as f64 + t * 1000.0;
        let i = c.timecodes.partition_point(|&tc| (tc as f64) < at);
        if c.timecodes.is_empty() {
            return None;
        }
        if i == 0 || i == c.timecodes.len() {
            return Some(value(&c.values, i.min(c.timecodes.len() - 1)));
        }
        let (ta, tb) = (c.timecodes[i - 1] as f64, c.timecodes[i] as f64);
        let (va, vb) = (value(&c.values, i - 1), value(&c.values, i));
        Some(if tb > ta { va + (vb - va) * (at - ta) / (tb - ta) } else { vb })
    }
}

/// Linear interpolation over non-decreasing `xs`, clamped at the ends.
pub(crate) fn interp(xs: &[f64], ys: &[f64], x: f64) -> f64 {
    let i = xs.partition_point(|&v| v < x);
    if i == 0 {
        return ys[0];
    }
    if i == xs.len() {
        return ys[xs.len() - 1];
    }
    let (xa, xb) = (xs[i - 1], xs[i]);
    if xb > xa { ys[i - 1] + (ys[i] - ys[i - 1]) * (x - xa) / (xb - xa) } else { ys[i] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_at_the_finish_is_the_lap_time_gap() {
        let t: Vec<f64> = (0..=100).map(|i| i as f64 * 0.1).collect(); // 10 s at 20 m/s = 200 m
        let slow = Trace::from_speed(&t, &[20.0; 101]);
        let fast = Trace::from_speed(&t[..81], &[25.0; 81]); // 8 s at 25 m/s = 200 m
        let len = fast.length();
        assert!((slow.time_at(len, len) - fast.time_at(len, len) - 2.0).abs() < 1e-9);
    }
}
