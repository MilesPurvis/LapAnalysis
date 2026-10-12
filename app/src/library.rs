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
use crate::info::{friendly_file_name, SessionInfo};

const INDEX_FILE: &str = "index.json";

/// Reads sample `k` of a channel as f64, whatever its stored type.
type Sampler<'a> = Box<dyn Fn(usize) -> f64 + 'a>;

#[derive(Clone, Default, Serialize, Deserialize)]
struct IndexEntry {
    /// The logger's summary row, captured when the file was downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<DeviceSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    downloaded_at: Option<u64>,
    /// What you've recorded about the session.
    #[serde(default)]
    info: SessionInfo,
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
    /// Your name for the session (empty if not set).
    pub title: String,
    pub size: u64,
    pub kind: &'static str,
    pub date: String,
    pub time: String,
    pub track: String,
    pub driver: String,
    pub vehicle: String,
    pub has_notes: bool,
    pub laps: usize,
    pub best_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub downloaded_at: Option<u64>,
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
    pub title: String,
    pub kind: &'static str,
    pub size: u64,
    /// Effective values: yours if set, else what the logger recorded.
    pub driver: String,
    pub vehicle: String,
    pub venue: String,
    /// The logger's own track name, for showing what an override replaces.
    pub logger_track: String,
    pub info: SessionInfo,
    /// Other library sessions recorded on the same day.
    pub same_day: usize,
    pub suggestions: Suggestions,
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

/// Values used before, most common first, for autocomplete.
#[derive(Default, Serialize)]
pub struct Suggestions {
    pub drivers: Vec<String>,
    pub vehicles: Vec<String>,
    pub tracks: Vec<String>,
    pub compounds: Vec<String>,
}

/// What the logger recorded for a session: the download-time summary row first, then the
/// file's own metadata.
#[derive(Default)]
struct Recorded {
    date: String,
    time: String,
    track: String,
    driver: String,
    vehicle: String,
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
        // A re-download keeps the details you've already entered.
        let entry = index.entry(device.name.clone()).or_default();
        entry.device = Some(device.clone());
        entry.downloaded_at = Some(downloaded_at);
        self.save_index(&index)
    }

    fn save_index(&self, index: &BTreeMap<String, IndexEntry>) -> Result<()> {
        let path = self.dir.join(INDEX_FILE);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(index)?)?;
        fs::rename(&tmp, &path).context("save library index.json")
    }

    /// What the logger recorded for every library file.
    fn recorded_all(&self) -> Result<Vec<(String, Recorded)>> {
        let index = self.index.lock().unwrap().clone();
        let mut out = vec![];
        for name in self.file_names()? {
            let loaded = self.load(&name)?;
            let rec = recorded(index.get(&name), &loaded);
            out.push((name, rec));
        }
        Ok(out)
    }

    /// Save your details for a session. With `fill_day`, also copy its driver, car and track
    /// into the other sessions recorded that day that have none yet (neither yours nor the
    /// logger's). Returns how many other sessions changed.
    pub fn update_info(&self, name: &str, info: SessionInfo, fill_day: bool) -> Result<usize> {
        if !self.contains(name) {
            bail!("no session {name}");
        }
        let info = info.cleaned()?;
        let all = if fill_day { self.recorded_all()? } else { vec![] };
        let day = all.iter().find(|(n, _)| n == name).map(|(_, r)| r.date.clone()).unwrap_or_default();
        let mut index = self.index.lock().unwrap();
        let mut filled = 0;
        for (other, rec) in all.iter().filter(|(n, r)| n != name && !day.is_empty() && r.date == day) {
            let e = index.entry(other.clone()).or_default();
            let before = e.info.clone();
            // A driver or car the logger recorded is kept. Its track name is usually a
            // truncated version of the same track, so yours replaces it.
            if e.info.driver.is_empty() && rec.driver.is_empty() {
                e.info.driver = info.driver.clone();
            }
            if e.info.vehicle.is_empty() && rec.vehicle.is_empty() {
                e.info.vehicle = info.vehicle.clone();
            }
            if e.info.track.is_empty() {
                e.info.track = info.track.clone();
            }
            filled += (e.info != before) as usize;
        }
        index.entry(name.to_string()).or_default().info = info;
        self.save_index(&index)?;
        Ok(filled)
    }

    pub fn suggestions(&self) -> Suggestions {
        let index = self.index.lock().unwrap();
        let infos = || index.values().map(|e| &e.info);
        let devices = || index.values().filter_map(|e| e.device.as_ref());
        Suggestions {
            drivers: ranked(infos().map(|i| i.driver.as_str()).chain(devices().map(|d| d.driver.as_str()))),
            vehicles: ranked(infos().map(|i| i.vehicle.as_str()).chain(devices().map(|d| d.vehicle.as_str()))),
            tracks: ranked(infos().map(|i| i.track.as_str()).chain(devices().map(|d| d.track.as_str()))),
            compounds: ranked(infos().map(|i| i.tyres.compound.as_str())),
        }
    }

    /// File name for downloading a session from the UI.
    pub fn download_name(&self, name: &str) -> String {
        let entry = self.index.lock().unwrap().get(name).cloned();
        let Ok(loaded) = self.load(name) else { return name.to_string() };
        let rec = recorded(entry.as_ref(), &loaded);
        let info = entry.map(|e| e.info).unwrap_or_default();
        friendly_file_name(name, &rec.date, first(&info.track, &rec.track), &info.name)
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
        let mut items = self.file_names()?.iter().map(|n| self.item(n)).collect::<Result<Vec<_>>>()?;
        items.sort_by(|a, b| (&b.date, &b.time, &b.name).cmp(&(&a.date, &a.time, &a.name)));
        Ok(items)
    }

    /// One library row: your details over what the logger recorded.
    pub fn item(&self, name: &str) -> Result<LibraryItem> {
        let size = fs::metadata(self.path(name)?)?.len();
        let loaded = self.load(name)?;
        let (laps, best) = loaded.xrk.as_ref().map(lap_stats).unwrap_or((0, None));
        let entry = self.index.lock().unwrap().get(name).cloned();
        let entry = entry.as_ref();
        let rec = recorded(entry, &loaded);
        let info = entry.map(|e| e.info.clone()).unwrap_or_default();
        let mut item = LibraryItem {
            kind: kind(&loaded),
            name: name.to_string(),
            title: info.name.clone(),
            size,
            track: first(&info.track, &rec.track).to_string(),
            driver: first(&info.driver, &rec.driver).to_string(),
            vehicle: first(&info.vehicle, &rec.vehicle).to_string(),
            has_notes: !info.notes.is_empty(),
            date: rec.date,
            time: rec.time,
            laps,
            best_ms: best,
            duration_ms: None,
            downloaded_at: entry.and_then(|e| e.downloaded_at),
        };
        if let Some(d) = entry.and_then(|e| e.device.as_ref()) {
            item.duration_ms = d.duration_ms;
            // Match the Logger tab, which counts the out-lap too.
            item.laps = d.laps as usize;
            item.best_ms = d.best_ms.or(item.best_ms);
        }
        if item.duration_ms.is_none() {
            item.duration_ms = loaded.xrk.as_ref().map(|x| (span_ms(x).1 - span_ms(x).0) as u64);
        }
        Ok(item)
    }

    pub fn detail(&self, name: &str) -> Result<SessionDetail> {
        let loaded = self.load(name)?;
        let size = fs::metadata(self.path(name)?)?.len();
        let entry = self.index.lock().unwrap().get(name).cloned();
        let rec = recorded(entry.as_ref(), &loaded);
        let info = entry.as_ref().map(|e| e.info.clone()).unwrap_or_default();
        let same_day = if rec.date.is_empty() {
            0
        } else {
            self.recorded_all()?.iter().filter(|(n, r)| n != name && r.date == rec.date).count()
        };
        let mut d = SessionDetail {
            name: name.to_string(),
            title: info.name.clone(),
            kind: kind(&loaded),
            size,
            driver: first(&info.driver, &rec.driver).to_string(),
            vehicle: first(&info.vehicle, &rec.vehicle).to_string(),
            venue: first(&info.track, &rec.track).to_string(),
            logger_track: rec.track.clone(),
            info,
            same_day,
            suggestions: self.suggestions(),
            date: rec.date,
            time: rec.time,
            logger: String::new(),
            duration_s: 0.0,
            laps: vec![],
            channels: vec![],
            has_gps: false,
            device: entry.and_then(|e| e.device),
            text: loaded.text.clone(),
            error: loaded.error.clone(),
            repaired_bytes: loaded.repaired_bytes,
        };
        let Some(x) = &loaded.xrk else { return Ok(d) };
        let m = &x.metadata;
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

fn first<'a>(mine: &'a str, logger: &'a str) -> &'a str {
    if mine.is_empty() { logger } else { mine }
}

fn recorded(entry: Option<&IndexEntry>, loaded: &Loaded) -> Recorded {
    let dev = entry.and_then(|e| e.device.as_ref());
    let meta = loaded.xrk.as_ref().map(|x| &x.metadata);
    let from_dev = |f: fn(&DeviceSession) -> &str| dev.map(f).unwrap_or("");
    let from_meta = |f: fn(&libxrk::Metadata) -> Option<&str>| meta.and_then(f).unwrap_or("");
    let meta_date = meta.and_then(|m| m.log_date.as_deref()).map(mdy_to_iso).unwrap_or_default();
    Recorded {
        date: first(from_dev(|d| &d.date), &meta_date).to_string(),
        time: first(from_dev(|d| &d.time), from_meta(|m| m.log_time.as_deref())).to_string(),
        track: first(from_dev(|d| &d.track), from_meta(|m| m.venue.as_deref())).to_string(),
        driver: first(from_dev(|d| &d.driver), from_meta(|m| m.driver.as_deref())).to_string(),
        vehicle: first(from_dev(|d| &d.vehicle), from_meta(|m| m.vehicle.as_deref())).to_string(),
    }
}

/// Distinct non-empty values, most frequent first (ties alphabetical), ignoring case.
fn ranked<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for v in values.map(str::trim).filter(|v| !v.is_empty()) {
        counts.entry(v.to_lowercase()).or_insert_with(|| (v.to_string(), 0)).1 += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_values().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().map(|(v, _)| v).collect()
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

pub(crate) fn value(v: &ChannelValues, i: usize) -> f64 {
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

pub(crate) fn laps(x: &XrkFile) -> Vec<Lap> {
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

pub(crate) fn lap_window(x: &XrkFile, lap: Option<i32>) -> Result<(i64, i64)> {
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

    fn row(name: &str) -> DeviceSession {
        let csv = format!("name,size,date,hour,track_name\r\n{name},0,07/09/2026,12:00:00,Great Bend T\r\n");
        crate::aim::parse_summary(&csv).remove(0)
    }

    fn temp_library(tag: &str) -> (PathBuf, Library) {
        let dir = std::env::temp_dir().join(format!("lapanalysis-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let lib = Library::open(&dir).unwrap();
        (dir, lib)
    }

    #[test]
    fn strips_debug_log_lines() {
        let mut raw = b"40041 I: Start Memo: 1:/MEM/A_0124.XRZ\r\n".to_vec();
        raw.extend_from_slice(&[0x78, 0x01, 0x9c]);
        raw.extend_from_slice(b"38766 I: LAP: n.1  1:42.419\r\n");
        raw.push(0xff);
        let (data, lines) = strip_log_lines(&raw);
        assert_eq!((data, lines.len()), (vec![0x78, 0x01, 0x9c, 0xff], 2));
    }

    #[test]
    fn saves_session_details() {
        let (dir, lib) = temp_library("details");
        lib.store(&row("a_1.xrz"), b"").unwrap();
        let info = SessionInfo { name: "Practice 2".into(), driver: "Miles".into(), ..Default::default() };
        lib.update_info("a_1.xrz", info, false).unwrap();
        let item = Library::open(&dir).unwrap().list().unwrap().remove(0);
        assert_eq!((item.title.as_str(), item.driver.as_str()), ("Practice 2", "Miles"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_an_older_index_file() {
        let (dir, _) = temp_library("compat");
        fs::write(dir.join("a_1.xrz"), b"").unwrap();
        let device = serde_json::to_string(&row("a_1.xrz")).unwrap();
        fs::write(dir.join(INDEX_FILE), format!(r#"{{"a_1.xrz":{{"device":{device},"downloaded_at":1}}}}"#)).unwrap();
        assert_eq!(Library::open(&dir).unwrap().list().unwrap()[0].track, "Great Bend T");
        fs::remove_dir_all(dir).unwrap();
    }
}
