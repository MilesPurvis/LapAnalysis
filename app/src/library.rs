//! Local session library: downloaded .xrz files, the device summary captured at download
//! time, and parsed sessions cached in memory.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use libxrk::{ChannelValues, XrkFile};
use serde::{Deserialize, Serialize};

use crate::aim::DeviceSession;

const INDEX_FILE: &str = "index.json";

/// Reads sample `k` of a channel as f64, whatever its stored type.
type Sampler<'a> = Box<dyn Fn(usize) -> f64 + 'a>;

#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    device: DeviceSession,
    downloaded_at: u64,
}

pub struct Library {
    dir: PathBuf,
    index: Mutex<BTreeMap<String, IndexEntry>>,
    cache: Mutex<HashMap<String, (SystemTime, Arc<Loaded>)>>,
}

/// A library file after parsing. Files the logger writes that aren't telemetry (its
/// text debug log shows up as a session name too) keep their text instead.
pub struct Loaded {
    pub xrk: Option<XrkFile>,
    /// The logger's debug-log text: the whole file for a log, or the lines stripped out of
    /// a session the firmware wrote them into.
    pub text: Option<String>,
    pub error: Option<String>,
    /// Bytes of debug-log text removed before parsing (0 = the file was clean).
    pub repaired_bytes: usize,
}

#[derive(Serialize)]
pub struct LibraryItem {
    pub name: String,
    pub size: u64,
    pub kind: &'static str,
    pub date: String,
    pub time: String,
    pub track: String,
    pub laps: usize,
    pub best_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub downloaded_at: Option<u64>,
    pub repaired: bool,
}

#[derive(Serialize)]
pub struct Lap {
    pub num: i32,
    pub kind: String,
    pub start_s: f64,
    pub end_s: f64,
    pub duration_ms: i64,
    pub best: bool,
}

#[derive(Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub units: String,
    pub group: String,
    pub samples: usize,
    pub rate_hz: f64,
}

#[derive(Serialize)]
pub struct SessionDetail {
    pub name: String,
    pub kind: &'static str,
    pub size: u64,
    pub driver: String,
    pub vehicle: String,
    pub venue: String,
    pub date: String,
    pub time: String,
    pub logger: String,
    pub duration_s: f64,
    pub laps: Vec<Lap>,
    pub channels: Vec<ChannelInfo>,
    pub has_gps: bool,
    pub device: Option<DeviceSession>,
    pub text: Option<String>,
    pub error: Option<String>,
    pub repaired_bytes: usize,
}

#[derive(Serialize)]
pub struct Series {
    pub id: String,
    pub name: String,
    pub units: String,
    pub t: Vec<f64>,
    pub v: Vec<f64>,
}

#[derive(Serialize)]
pub struct Track {
    pub t: Vec<f64>,
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub speed_kmh: Vec<f64>,
}

impl Library {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let index = match fs::read(dir.join(INDEX_FILE)) {
            Ok(raw) => serde_json::from_slice(&raw).context("parse library index.json")?,
            Err(_) => BTreeMap::new(),
        };
        Ok(Self { dir, index: Mutex::new(index), cache: Mutex::new(HashMap::new()) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            bail!("bad session name {name:?}");
        }
        Ok(self.dir.join(name))
    }

    /// Session files in the library (.xrz/.xrk), unparsed.
    pub fn file_names(&self) -> Result<Vec<String>> {
        let mut names = vec![];
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".xrz") || lower.ends_with(".xrk") {
                names.push(name);
            }
        }
        Ok(names)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.path(name).is_ok_and(|p| p.is_file())
    }

    /// Store a downloaded file atomically, with the device's summary row.
    pub fn store(&self, device: &DeviceSession, data: &[u8]) -> Result<()> {
        let path = self.path(&device.name)?;
        let tmp = path.with_extension("part");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        let downloaded_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut index = self.index.lock().unwrap();
        index.insert(device.name.clone(), IndexEntry { device: device.clone(), downloaded_at });
        fs::write(self.dir.join(INDEX_FILE), serde_json::to_vec_pretty(&*index)?)?;
        Ok(())
    }

    pub fn raw(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.path(name)?;
        fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    pub fn load(&self, name: &str) -> Result<Arc<Loaded>> {
        let path = self.path(name)?;
        let mtime = fs::metadata(&path).with_context(|| format!("no session {name}"))?.modified()?;
        if let Some((t, loaded)) = self.cache.lock().unwrap().get(name) {
            if *t == mtime {
                return Ok(loaded.clone());
            }
        }
        let raw = fs::read(&path)?;
        let loaded = Arc::new(parse(&raw));
        self.cache.lock().unwrap().insert(name.to_string(), (mtime, loaded.clone()));
        Ok(loaded)
    }

    pub fn list(&self) -> Result<Vec<LibraryItem>> {
        let index = self.index.lock().unwrap().clone();
        let mut items = vec![];
        for name in self.file_names()? {
            let size = fs::metadata(self.path(&name)?)?.len();
            let loaded = self.load(&name)?;
            let (laps, best) = loaded.xrk.as_ref().map(lap_stats).unwrap_or((0, None));
            let mut item = LibraryItem {
                kind: kind(&loaded),
                name: name.clone(),
                size,
                date: String::new(),
                time: String::new(),
                track: String::new(),
                laps,
                best_ms: best,
                duration_ms: None,
                downloaded_at: None,
                repaired: loaded.xrk.is_some() && loaded.repaired_bytes > 0,
            };
            if let Some(e) = index.get(&name) {
                item.date = e.device.date.clone();
                item.time = e.device.time.clone();
                item.track = e.device.track.clone();
                item.duration_ms = e.device.duration_ms;
                item.downloaded_at = Some(e.downloaded_at);
                // Match the Logger tab, which counts the out-lap too.
                item.laps = e.device.laps as usize;
                item.best_ms = e.device.best_ms.or(item.best_ms);
            } else if let Some(x) = &loaded.xrk {
                item.date = x.metadata.log_date.as_deref().map(mdy_to_iso).unwrap_or_default();
                item.time = x.metadata.log_time.clone().unwrap_or_default();
                item.track = x.metadata.venue.clone().unwrap_or_default();
            }
            if item.duration_ms.is_none() {
                item.duration_ms = loaded.xrk.as_ref().map(|x| (span_ms(x).1 - span_ms(x).0) as u64);
            }
            items.push(item);
        }
        items.sort_by(|a, b| (&b.date, &b.time, &b.name).cmp(&(&a.date, &a.time, &a.name)));
        Ok(items)
    }

    pub fn detail(&self, name: &str) -> Result<SessionDetail> {
        let loaded = self.load(name)?;
        let size = fs::metadata(self.path(name)?)?.len();
        let device = self.index.lock().unwrap().get(name).map(|e| e.device.clone());
        let mut d = SessionDetail {
            name: name.to_string(),
            kind: kind(&loaded),
            size,
            driver: String::new(),
            vehicle: String::new(),
            venue: String::new(),
            date: String::new(),
            time: String::new(),
            logger: String::new(),
            duration_s: 0.0,
            laps: vec![],
            channels: vec![],
            has_gps: false,
            device,
            text: loaded.text.clone(),
            error: loaded.error.clone(),
            repaired_bytes: loaded.repaired_bytes,
        };
        let Some(x) = &loaded.xrk else { return Ok(d) };
        let m = &x.metadata;
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        d.driver = s(&m.driver);
        d.vehicle = s(&m.vehicle);
        d.venue = s(&m.venue);
        d.date = m.log_date.as_deref().map(mdy_to_iso).unwrap_or_default();
        d.time = s(&m.log_time);
        d.logger = m.logger_model.clone().or_else(|| m.device_name.clone()).unwrap_or_default();
        let (t0, t1) = span_ms(x);
        d.duration_s = (t1 - t0) as f64 / 1000.0;
        d.laps = laps(x);
        d.channels = channels(x);
        d.has_gps = x.gps.as_ref().is_some_and(|g| !g.timecodes.is_empty());
        Ok(d)
    }

    pub fn series(&self, name: &str, id: &str, lap: Option<i32>, points: usize) -> Result<Series> {
        let loaded = self.load(name)?;
        let x = loaded.xrk.as_ref().context("not a telemetry session")?;
        let (t0, t1) = lap_window(x, lap)?;
        let (name, units, times, values): (String, String, &[i64], Sampler) =
            if let Some(i) = id.strip_prefix('c').and_then(|s| s.parse::<usize>().ok()) {
                let c = x.channels.get(i).context("no such channel")?;
                (c.name.clone(), c.units.clone(), &c.timecodes, Box::new(move |k| value(&c.values, k)))
            } else {
                let g = x.gps.as_ref().context("session has no GPS")?;
                match id {
                    "gps:speed" => ("GPS Speed".into(), "km/h".into(), &g.timecodes, Box::new(move |k| g.speed[k] * 3.6)),
                    "gps:alt" => ("GPS Altitude".into(), "m".into(), &g.timecodes, Box::new(move |k| g.altitude[k])),
                    "gps:sats" => ("GPS Satellites".into(), String::new(), &g.timecodes, Box::new(move |k| g.satellites[k] as f64)),
                    _ => bail!("unknown channel {id}"),
                }
            };
        let (lo, hi) = window_indices(times, t0, t1);
        let (t, v) = decimate_minmax(&times[lo..hi], |k| values(lo + k), t0, points);
        Ok(Series { id: id.to_string(), name, units, t, v })
    }

    pub fn track(&self, name: &str, lap: Option<i32>, points: usize) -> Result<Track> {
        let loaded = self.load(name)?;
        let x = loaded.xrk.as_ref().context("not a telemetry session")?;
        let g = x.gps.as_ref().context("session has no GPS")?;
        let (t0, t1) = lap_window(x, lap)?;
        let (lo, hi) = window_indices(&g.timecodes, t0, t1);
        let step = ((hi - lo) / points.max(1)).max(1);
        let mut tr = Track { t: vec![], lat: vec![], lon: vec![], speed_kmh: vec![] };
        for k in (lo..hi).step_by(step) {
            let (lat, lon) = (g.latitude[k], g.longitude[k]);
            if lat == 0.0 && lon == 0.0 || !lat.is_finite() || !lon.is_finite() {
                continue; // no fix yet
            }
            tr.t.push((g.timecodes[k] - t0) as f64 / 1000.0);
            tr.lat.push(lat);
            tr.lon.push(lon);
            tr.speed_kmh.push(g.speed[k] * 3.6);
        }
        Ok(tr)
    }
}

fn parse(raw: &[u8]) -> Loaded {
    let (data, log) = strip_log_lines(raw);
    let repaired_bytes = raw.len() - data.len();
    let text = (!log.is_empty()).then(|| log.join("\n"));
    if data.iter().all(u8::is_ascii_whitespace) {
        return Loaded { xrk: None, text, error: None, repaired_bytes };
    }
    match libxrk::read_xrk(&data) {
        Ok(x) if !x.channels.is_empty() || x.gps.is_some() => Loaded { xrk: Some(x), text, error: None, repaired_bytes },
        Ok(_) => Loaded { xrk: None, text, error: Some("file has no channels".into()), repaired_bytes },
        Err(e) => Loaded { xrk: None, text, error: Some(e.to_string()), repaired_bytes },
    }
}

/// Length of a Solo debug-log line at the start of `d`, e.g. `40041 I: Start Memo: …\r\n`:
/// a 5-wide millisecond counter, a 1–2 letter level, then printable text and CRLF.
fn log_line_len(d: &[u8]) -> Option<usize> {
    if d.len() < 10 || !d[..4].iter().all(|b| *b == b' ' || b.is_ascii_digit()) || !d[4].is_ascii_digit() || d[5] != b' ' {
        return None;
    }
    let mut i = 6;
    while i < 8 && d.get(i).is_some_and(u8::is_ascii_uppercase) {
        i += 1;
    }
    if i == 6 || d.get(i..i + 2) != Some(b": ") {
        return None;
    }
    i += 2;
    while d.get(i).is_some_and(|b| (0x20..0x7f).contains(b)) {
        i += 1;
    }
    (d.get(i..i + 2) == Some(b"\r\n")).then_some(i + 2)
}

/// The Solo's firmware sometimes writes its debug log into the session file it's recording
/// (its own log reports "SD log error"): the lines land between chunks of the zlib stream,
/// and the logger's own debug log can even be saved as a "session". Removing them restores
/// a stream whose zlib checksum passes. Returns the cleaned bytes and the removed lines.
fn strip_log_lines(raw: &[u8]) -> (Vec<u8>, Vec<String>) {
    let mut out = Vec::with_capacity(raw.len());
    let mut lines = vec![];
    let mut i = 0;
    while i < raw.len() {
        match log_line_len(&raw[i..]) {
            Some(n) => {
                lines.push(String::from_utf8_lossy(&raw[i..i + n - 2]).trim_start().to_string());
                i += n;
            }
            None => {
                out.push(raw[i]);
                i += 1;
            }
        }
    }
    (out, lines)
}

fn kind(l: &Loaded) -> &'static str {
    match (&l.xrk, &l.text, &l.error) {
        (Some(_), _, _) => "session",
        (None, Some(_), None) => "log",
        _ => "unreadable",
    }
}

fn value(v: &ChannelValues, i: usize) -> f64 {
    match v {
        ChannelValues::UInt8(a) => a[i] as f64,
        ChannelValues::UInt16(a) => a[i] as f64,
        ChannelValues::Int16(a) => a[i] as f64,
        ChannelValues::Int32(a) => a[i] as f64,
        ChannelValues::UInt32(a) => a[i] as f64,
        ChannelValues::Float32(a) => a[i] as f64,
        ChannelValues::Float64(a) => a[i],
    }
}

fn span_ms(x: &XrkFile) -> (i64, i64) {
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    let gps = x.gps.iter().map(|g| &g.timecodes);
    for tc in x.channels.iter().map(|c| &c.timecodes).chain(gps) {
        if let (Some(a), Some(b)) = (tc.first(), tc.last()) {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
    }
    for l in &x.laps {
        lo = lo.min(l.start_time);
        hi = hi.max(l.end_time);
    }
    if lo > hi { (0, 0) } else { (lo, hi) }
}

fn best_full_lap(x: &XrkFile) -> Option<(i32, i64)> {
    x.laps
        .iter()
        .filter(|l| l.lap_type == "full")
        .map(|l| (l.num, l.end_time - l.start_time))
        .min_by_key(|(_, d)| *d)
}

fn lap_stats(x: &XrkFile) -> (usize, Option<u64>) {
    let full = x.laps.iter().filter(|l| l.lap_type == "full").count();
    (full, best_full_lap(x).map(|(_, d)| d as u64))
}

fn laps(x: &XrkFile) -> Vec<Lap> {
    let best = best_full_lap(x).map(|(n, _)| n);
    x.laps
        .iter()
        .map(|l| Lap {
            num: l.num,
            kind: l.lap_type.clone(),
            start_s: l.start_time as f64 / 1000.0,
            end_s: l.end_time as f64 / 1000.0,
            duration_ms: l.end_time - l.start_time,
            best: Some(l.num) == best,
        })
        .collect()
}

fn channels(x: &XrkFile) -> Vec<ChannelInfo> {
    let rate = |tc: &[i64]| match (tc.first(), tc.last()) {
        (Some(a), Some(b)) if b > a && tc.len() > 1 => (tc.len() - 1) as f64 * 1000.0 / (b - a) as f64,
        _ => 0.0,
    };
    let mut out: Vec<ChannelInfo> = vec![];
    if let Some(g) = x.gps.as_ref().filter(|g| !g.timecodes.is_empty()) {
        for (id, name, units) in [("gps:speed", "GPS Speed", "km/h"), ("gps:alt", "GPS Altitude", "m"), ("gps:sats", "GPS Satellites", "")] {
            out.push(ChannelInfo {
                id: id.into(),
                name: name.into(),
                units: units.into(),
                group: "GPS".into(),
                samples: g.timecodes.len(),
                rate_hz: rate(&g.timecodes),
            });
        }
    }
    for (i, c) in x.channels.iter().enumerate() {
        out.push(ChannelInfo {
            id: format!("c{i}"),
            name: c.name.clone(),
            units: c.units.clone(),
            group: c.group.clone().unwrap_or_else(|| "Logger".into()),
            samples: c.timecodes.len(),
            rate_hz: rate(&c.timecodes),
        });
    }
    out
}

fn lap_window(x: &XrkFile, lap: Option<i32>) -> Result<(i64, i64)> {
    match lap {
        None => Ok(span_ms(x)),
        Some(n) => {
            let l = x.laps.iter().find(|l| l.num == n).with_context(|| format!("no lap {n}"))?;
            Ok((l.start_time, l.end_time))
        }
    }
}

fn window_indices(times: &[i64], t0: i64, t1: i64) -> (usize, usize) {
    (times.partition_point(|&t| t < t0), times.partition_point(|&t| t <= t1))
}

/// Keep each bucket's min and max (in time order) so peaks survive downsampling.
/// Times come back in seconds relative to `t0`.
fn decimate_minmax(times: &[i64], value: impl Fn(usize) -> f64, t0: i64, points: usize) -> (Vec<f64>, Vec<f64>) {
    let n = times.len();
    let secs = |k: usize| (times[k] - t0) as f64 / 1000.0;
    let points = points.max(2);
    if n <= points * 2 {
        return ((0..n).map(secs).collect(), (0..n).map(&value).collect());
    }
    let bucket = n.div_ceil(points);
    let (mut t, mut v) = (Vec::with_capacity(points * 2), Vec::with_capacity(points * 2));
    for start in (0..n).step_by(bucket) {
        let end = (start + bucket).min(n);
        let (mut kmin, mut kmax) = (start, start);
        let (mut vmin, mut vmax) = (value(start), value(start));
        for k in start + 1..end {
            let x = value(k);
            if x < vmin {
                (kmin, vmin) = (k, x);
            }
            if x > vmax {
                (kmax, vmax) = (k, x);
            }
        }
        for k in if kmin <= kmax { [kmin, kmax] } else { [kmax, kmin] } {
            if t.last() != Some(&secs(k)) {
                t.push(secs(k));
                v.push(value(k));
            }
        }
    }
    (t, v)
}

fn mdy_to_iso(mdy: &str) -> String {
    match mdy.split('/').collect::<Vec<_>>()[..] {
        [m, d, y] => format!("{y}-{m:0>2}-{d:0>2}"),
        _ => mdy.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimation_keeps_extremes_and_order() {
        let times: Vec<i64> = (0..10_000).map(|i| i * 10).collect();
        let vals: Vec<f64> = (0..10_000).map(|i| if i == 4321 { 99.0 } else { (i % 7) as f64 }).collect();
        let (t, v) = decimate_minmax(&times, |k| vals[k], 0, 100);
        assert!(t.len() <= 200);
        assert!(t.windows(2).all(|w| w[0] < w[1]));
        assert!(v.contains(&99.0));
    }

    #[test]
    fn short_series_pass_through() {
        let times = [0, 100, 200];
        let (t, v) = decimate_minmax(&times, |k| k as f64, 0, 100);
        assert_eq!((t, v), (vec![0.0, 0.1, 0.2], vec![0.0, 1.0, 2.0]));
    }

    #[test]
    fn strips_debug_log_lines_from_binary_data() {
        // Shapes taken from a_0124.xrz: a log prefix before the zlib header, and a block of
        // lines glued straight onto binary bytes (no separator) mid-file.
        let mut raw = b"39789 I: PreBuff Size: 0x00002bd1\r\n40041 I: Start Memo: 1:/MEM/A_0124.XRZ\r\n".to_vec();
        raw.extend_from_slice(&[0x78, 0x01, 0x9c, 0x7d, b'9', 0xf4]);
        raw.extend_from_slice(b"99666 I: BEST LAP\r\n  125 CR: take_mutex [162]: get FS mutex\r\n83856 N: IOE: 0x02\r\n");
        raw.extend_from_slice(&[0x00, b'1', b'2', b' ', b'I', 0x0d, 0x0a, 0xff]);
        let (data, lines) = strip_log_lines(&raw);
        assert_eq!(data, [0x78, 0x01, 0x9c, 0x7d, b'9', 0xf4, 0x00, b'1', b'2', b' ', b'I', 0x0d, 0x0a, 0xff]);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[3], "125 CR: take_mutex [162]: get FS mutex");
    }

    #[test]
    fn clean_data_is_untouched() {
        let raw: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert_eq!(strip_log_lines(&raw).0, raw);
    }

    #[test]
    fn xrk_dates_are_month_first() {
        assert_eq!(mdy_to_iso("09/07/2026"), "2026-09-07");
    }
}
