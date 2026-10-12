//! Theoretical best lap: the fastest time through each corner across the full laps of the
//! sessions you pick, added up. Corners come from the fastest lap's speed trace and are
//! numbered from the start/finish line (T1, T2, …); every lap is lined up by distance as in
//! lap comparison.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::compare::{catalog, interp, PreparedLap, SessionLaps, GPS_SPEED};
use crate::library::Library;

/// Points along the lap the speed trace and racing line are sampled at.
const GRID: usize = 1200;
/// A speed minimum counts as a corner when speed drops at least this much into it and
/// rises at least this much out of it, so fast kinks and GPS noise don't.
const MIN_PROMINENCE_KMH: f64 = 6.0;
/// Minima closer than this are one corner.
const MIN_CORNER_GAP_M: f64 = 80.0;
const SMOOTH_M: f64 = 15.0;
/// Laps whose GPS distance is this far off the median are left out (bad fixes, a cut track).
const LENGTH_TOLERANCE: f64 = 0.05;
/// Sessions whose first GPS fixes are this close count as the same track.
const SAME_TRACK_KM: f64 = 3.0;

#[derive(Serialize)]
pub struct TheoreticalBest {
    /// Sessions whose laps were used.
    pub sessions: Vec<String>,
    /// Sessions left out because they're at a different track (or have no GPS).
    pub skipped: Vec<String>,
    pub laps: usize,
    pub theoretical_ms: f64,
    /// The fastest actual lap; corners are measured on it.
    pub best: Holder,
    pub sections: Vec<Section>,
    /// Laps that hold at least one section; `Section::holder` indexes this.
    pub holders: Vec<Holder>,
    /// The best lap's racing line, and which section each point is in.
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub section_of: Vec<usize>,
}

#[derive(Clone, Serialize)]
pub struct Holder {
    pub file: String,
    pub title: String,
    pub driver: String,
    pub vehicle: String,
    pub lap: i32,
    pub lap_ms: i64,
}

#[derive(Serialize)]
pub struct Section {
    pub name: String,
    pub start_m: f64,
    pub end_m: f64,
    /// Grid index of the corner's slowest point, for labelling the map.
    pub apex: usize,
    pub best_ms: f64,
    pub holder: usize,
    /// How much longer the best lap took through this section than the fastest time.
    pub best_lap_loss_ms: f64,
}

/// Theoretical best over the full laps of `files`, which must be at the same track as the
/// first one; others are reported in `skipped`.
pub fn theoretical_best(lib: &Library, files: &[String]) -> Result<TheoreticalBest> {
    let catalog = catalog(lib)?;
    let first = files.first().context("pick at least one session")?;
    let anchor = catalog.iter().find(|s| &s.session.name == first).with_context(|| format!("{first} has no GPS"))?;
    let (mut chosen, mut skipped): (Vec<&SessionLaps>, Vec<String>) = (vec![], vec![]);
    for f in files {
        match catalog.iter().find(|s| &s.session.name == f) {
            Some(s) if same_track(s.position, anchor.position) => chosen.push(s),
            _ => skipped.push(f.clone()),
        }
    }
    let mut laps: Vec<PreparedLap> = chosen
        .iter()
        .flat_map(|s| s.laps.iter().filter(|l| l.kind == "full").map(move |l| (s.session.name.as_str(), l.num)))
        .filter_map(|(f, n)| PreparedLap::new(lib, f, n).ok())
        .collect();
    if laps.is_empty() {
        bail!("no full laps to work from (out and in laps don't count)");
    }
    let mut lengths: Vec<f64> = laps.iter().map(|l| l.trace.length()).collect();
    lengths.sort_by(f64::total_cmp);
    let median = lengths[lengths.len() / 2];
    laps.retain(|l| (l.trace.length() / median - 1.0).abs() <= LENGTH_TOLERANCE);

    let best_i = (0..laps.len()).min_by_key(|&i| laps[i].lap_ms).context("no usable laps")?;
    let best = &laps[best_i];
    let len = best.trace.length();
    let distance: Vec<f64> = (0..GRID).map(|i| len * i as f64 / (GRID - 1) as f64).collect();
    let best_times: Vec<f64> = distance.iter().map(|&d| best.trace.time_at(d, len)).collect();
    let speed: Vec<f64> = best_times.iter().map(|&t| best.sample(GPS_SPEED, t).unwrap_or(0.0)).collect();
    let (bounds, apexes) = corners(&distance, &speed);

    // Seconds each lap spent in each section.
    let times: Vec<Vec<f64>> = laps
        .iter()
        .map(|l| bounds.windows(2).map(|w| l.trace.time_at(distance[w[1]], len) - l.trace.time_at(distance[w[0]], len)).collect())
        .collect();
    let mut holders: Vec<usize> = vec![];
    let mut sections = vec![];
    for (j, w) in bounds.windows(2).enumerate() {
        let k = (0..laps.len()).min_by(|&a, &b| times[a][j].total_cmp(&times[b][j])).unwrap();
        let holder = holders.iter().position(|&h| h == k).unwrap_or_else(|| {
            holders.push(k);
            holders.len() - 1
        });
        sections.push(Section {
            name: if apexes.is_empty() { "Lap".into() } else { format!("T{}", j + 1) },
            start_m: distance[w[0]],
            end_m: distance[w[1]],
            apex: apexes.get(j).copied().unwrap_or((w[0] + w[1]) / 2),
            best_ms: times[k][j] * 1000.0,
            holder,
            best_lap_loss_ms: (times[best_i][j] - times[k][j]) * 1000.0,
        });
    }
    let holder = |l: &PreparedLap| Holder {
        file: l.session.name.clone(),
        title: l.session.title.clone(),
        driver: l.session.driver.clone(),
        vehicle: l.session.vehicle.clone(),
        lap: l.lap,
        lap_ms: l.lap_ms,
    };
    let section_of = (0..GRID).map(|i| bounds.windows(2).position(|w| i < w[1]).unwrap_or(bounds.len() - 2)).collect();
    Ok(TheoreticalBest {
        sessions: chosen.iter().map(|s| s.session.name.clone()).collect(),
        skipped,
        laps: laps.len(),
        theoretical_ms: sections.iter().map(|s| s.best_ms).sum(),
        best: holder(best),
        holders: holders.iter().map(|&k| holder(&laps[k])).collect(),
        lat: best_times.iter().map(|&t| interp(&best.gps_t, &best.lat, t)).collect(),
        lon: best_times.iter().map(|&t| interp(&best.gps_t, &best.lon, t)).collect(),
        section_of,
        sections,
    })
}

fn same_track(a: Option<(f64, f64)>, b: Option<(f64, f64)>) -> bool {
    let (Some(a), Some(b)) = (a, b) else { return false };
    let r = std::f64::consts::PI / 180.0;
    let km = 6371.0 * ((b.0 - a.0) * r).hypot((b.1 - a.1) * r * (a.0 * r).cos());
    km < SAME_TRACK_KM
}

/// Section boundaries (grid indices, first and last included) and corner apexes from a speed
/// trace. A corner is a speed minimum that stands out by `MIN_PROMINENCE_KMH`; sections split
/// at the fastest point between two corners.
fn corners(distance: &[f64], speed: &[f64]) -> (Vec<usize>, Vec<usize>) {
    let n = speed.len();
    let half = ((SMOOTH_M / (distance[1] - distance[0])).round() as usize).max(1);
    let smooth: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            speed[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let mut apexes: Vec<usize> = vec![];
    for i in (1..n - 1).filter(|&i| smooth[i] <= smooth[i - 1] && smooth[i] < smooth[i + 1] && prominence(&smooth, i) >= MIN_PROMINENCE_KMH) {
        match apexes.last_mut() {
            Some(last) if distance[i] - distance[*last] < MIN_CORNER_GAP_M => {
                if smooth[i] < smooth[*last] {
                    *last = i;
                }
            }
            _ => apexes.push(i),
        }
    }
    let mut bounds = vec![0];
    for w in apexes.windows(2) {
        bounds.push((w[0]..=w[1]).max_by(|&a, &b| smooth[a].total_cmp(&smooth[b])).unwrap());
    }
    bounds.push(n - 1);
    (bounds, apexes)
}

/// How far speed rises on either side of the minimum at `i` before dropping below it again
/// (the smaller side), i.e. how much the corner stands out.
fn prominence(s: &[f64], i: usize) -> f64 {
    let left = s[..i].iter().rev().take_while(|&&v| v >= s[i]).fold(s[i], |m, &v| m.max(v));
    let right = s[i + 1..].iter().take_while(|&&v| v >= s[i]).fold(s[i], |m, &v| m.max(v));
    left.min(right) - s[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_two_corners_in_a_two_corner_lap() {
        let distance: Vec<f64> = (0..1000).map(f64::from).collect();
        let dip = |d: f64, at: f64, depth: f64| depth * (-((d - at) / 30.0).powi(2)).exp();
        let speed: Vec<f64> = distance.iter().map(|&d| 120.0 - dip(d, 300.0, 50.0) - dip(d, 700.0, 30.0)).collect();
        let (bounds, apexes) = corners(&distance, &speed);
        assert_eq!(apexes.len(), 2);
        assert_eq!(bounds.len(), 3);
    }
}
