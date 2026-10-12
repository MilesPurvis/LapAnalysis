//! Client for the AiM Solo 2 DL's Wi-Fi protocol, as RaceStudio 3 speaks it.
//!
//! Reverse-engineered from the RS3 decompile and verified on the device (see LOG.md):
//!
//! - Discovery: UDP 36002. Send `aim-ka`; the logger answers with a 236-byte keep-alive
//!   (u32 length, u32 version, IP prefix at 8..11, busy flag `0x0e` bit 0, session-owner
//!   host octet at `0x11`, logger code at `0x5c`).
//! - Session: TCP 2000. Every write is framed
//!   `'<' 'h' tag:4 len:u32 0 '>' | data | '<' tag:4 sum16(data) '>'` with tag `STCP`
//!   (or `STNC` for command headers); every read carries `STCP`. The 8-byte hello is
//!   always framed, and its answer says whether the rest of the session stays framed.
//! - Commands: 64-byte header (cmd at `+0x08`, flags `+0x18`, ≤32 bytes of inline params
//!   at `+0x20`) → 64-byte ack → 64-byte reply header (data length `+0x10`, chunk size
//!   `+0x14`) → data pulled by sending u32 offsets, each answered with offset + chunk.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

pub const DEFAULT_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
pub const UDP_PORT: u16 = 36002;
pub const TCP_PORT: u16 = 2000;

const TAG_STCP: &[u8; 4] = b"STCP";
const TAG_STNC: &[u8; 4] = b"STNC";
const HELLO: [u8; 8] = [0, 0, 0, 0, 0x06, 0x08, 0, 0];

const CMD_IDENTIFY: u32 = 0x10002;
const CMD_HW_INFO: u32 = 0x10008;
const CMD_SUMMARY: u32 = 0x20024;
const CMD_READ_FILE: u32 = 0x40002;

/// Where the recorded sessions live on the device (from cmd 0x40140 ",recorded").
const RECORDED_DIR: &str = "1:/mem";

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub ip: Ipv4Addr,
    pub tcp_port: u16,
    pub udp_port: u16,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self { ip: DEFAULT_IP, tcp_port: TCP_PORT, udp_port: UDP_PORT }
    }
}

// --- discovery / keep-alive ---------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct Announce {
    pub ip: Ipv4Addr,
    pub logger_code: u32,
    pub busy: bool,
    /// Last octet of the host the logger thinks owns its TCP session (0 = none).
    pub owner: u8,
}

pub fn parse_keepalive(buf: &[u8], from: Ipv4Addr) -> Option<Announce> {
    if buf.len() != 0xec || u32::from_le_bytes(buf[0..4].try_into().ok()?) != 0xec {
        return None;
    }
    Some(Announce {
        ip: from,
        logger_code: u32::from_le_bytes(buf[0x5c..0x60].try_into().ok()?),
        busy: buf[0x0e] & 1 != 0,
        owner: buf[0x11],
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceStatus {
    pub online: bool,
    pub last: Option<Announce>,
    pub seconds_since_seen: Option<f64>,
    pub discovery_error: Option<String>,
}

/// Background `aim-ka` sender/listener, like RS3's CThreadGatherDeviceSuRete.
///
/// It only transmits while someone has shown interest recently (the UI polling status, or
/// a device operation), so a long-running server stays quiet on the home network.
pub struct KeepAlive {
    last: Arc<Mutex<Option<(Announce, Instant)>>>,
    interest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    error: Option<String>,
}

const INTEREST_WINDOW_MS: u64 = 15_000;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl KeepAlive {
    pub fn start(ep: &Endpoint, local_port: u16) -> Self {
        let last = Arc::new(Mutex::new(None));
        let interest = Arc::new(AtomicU64::new(now_ms()));
        let stop = Arc::new(AtomicBool::new(false));
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, local_port))
            .and_then(|s| s.set_broadcast(true).map(|_| s))
            .and_then(|s| s.set_read_timeout(Some(Duration::from_millis(200))).map(|_| s));
        let sock = match sock {
            Ok(s) => s,
            Err(e) => {
                let error = format!("can't bind UDP {local_port} for discovery: {e}");
                eprintln!("warning: {error}; assuming the Solo is at {}", ep.ip);
                return Self { last, interest, stop, error: Some(error) };
            }
        };
        let (l, i, s, ep) = (last.clone(), interest.clone(), stop.clone(), ep.clone());
        thread::spawn(move || keepalive_loop(sock, ep, l, i, s));
        Self { last, interest, stop, error: None }
    }

    pub fn touch(&self) {
        self.interest.store(now_ms(), Ordering::Relaxed);
    }

    pub fn status(&self) -> DeviceStatus {
        self.touch();
        let last = self.last.lock().unwrap().clone();
        let since = last.as_ref().map(|(_, t)| t.elapsed().as_secs_f64());
        DeviceStatus {
            online: since.is_some_and(|s| s < 5.0),
            last: last.map(|(a, _)| a),
            seconds_since_seen: since,
            discovery_error: self.error.clone(),
        }
    }

    /// The logger's address if it has answered recently.
    pub fn found_ip(&self) -> Option<Ipv4Addr> {
        let last = self.last.lock().unwrap();
        last.as_ref().filter(|(_, t)| t.elapsed() < Duration::from_secs(5)).map(|(a, _)| a.ip)
    }

    /// Block until the logger answers, or the timeout passes.
    pub fn wait_for_device(&self, timeout: Duration) -> Option<Announce> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            self.touch();
            if let Some((a, t)) = self.last.lock().unwrap().clone() {
                if t.elapsed() < Duration::from_secs(5) {
                    return Some(a);
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        None
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn keepalive_loop(
    sock: UdpSocket,
    ep: Endpoint,
    last: Arc<Mutex<Option<(Announce, Instant)>>>,
    interest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let mut next_send = Instant::now();
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let wanted = now_ms().saturating_sub(interest.load(Ordering::Relaxed)) < INTEREST_WINDOW_MS;
        if wanted && Instant::now() >= next_send {
            let known = last.lock().unwrap().as_ref().map(|(a, _)| a.ip);
            let mut targets = vec![ep.ip, Ipv4Addr::BROADCAST];
            targets.extend(known.filter(|ip| *ip != ep.ip));
            for ip in targets {
                let _ = sock.send_to(b"aim-ka", SocketAddrV4::new(ip, ep.udp_port));
            }
            next_send = Instant::now() + Duration::from_millis(500);
        }
        match sock.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) => {
                if let Some(a) = parse_keepalive(&buf[..n], *from.ip()) {
                    *last.lock().unwrap() = Some((a, Instant::now()));
                }
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

// --- framing --------------------------------------------------------------------------

fn sum16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |acc, &b| acc.wrapping_add(b as u16))
}

pub fn frame(data: &[u8], tag: &[u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 20);
    out.extend_from_slice(b"<h");
    out.extend_from_slice(tag);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0, b'>']);
    out.extend_from_slice(data);
    out.push(b'<');
    out.extend_from_slice(tag);
    out.extend_from_slice(&sum16(data).to_le_bytes());
    out.push(b'>');
    out
}

/// Validate an `STCP` frame carrying exactly `n` data bytes.
pub fn unframe(buf: &[u8], n: usize) -> Result<&[u8]> {
    if buf.len() != n + 20 || &buf[..2] != b"<h" || buf[11] != b'>' || &buf[2..6] != TAG_STCP {
        bail!("bad frame header {:02x?}", &buf[..buf.len().min(12)]);
    }
    let len = u32::from_le_bytes(buf[6..10].try_into()?) as usize;
    if len != n {
        bail!("frame carries {len} bytes, expected {n}");
    }
    let (data, tail) = (&buf[12..12 + n], &buf[12 + n..]);
    if tail[0] != b'<' || tail[7] != b'>' || &tail[1..5] != TAG_STCP {
        bail!("bad frame trailer {tail:02x?}");
    }
    if u16::from_le_bytes([tail[5], tail[6]]) != sum16(data) {
        bail!("frame checksum mismatch");
    }
    Ok(data)
}

// --- session ----------------------------------------------------------------------------

#[derive(Debug)]
pub struct Rejected {
    pub cmd: u32,
    pub flags: u8,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let why = if self.flags & 0x20 != 0 { "busy" } else { "invalid command" };
        write!(f, "logger rejected cmd 0x{:x} ({why}, flags 0x{:02x})", self.cmd, self.flags)
    }
}

impl std::error::Error for Rejected {}

pub struct Session {
    stream: TcpStream,
    framed: bool,
}

/// Progress callback: (bytes so far, total bytes).
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

impl Session {
    /// Connect and handshake with RS3's ESP32 timing, retrying once with slower timing.
    pub fn open(ep: &Endpoint) -> Result<Self> {
        match Self::open_with(ep, Duration::from_millis(400), 5, Duration::from_millis(750)) {
            Ok(s) => Ok(s),
            Err(first) => {
                thread::sleep(Duration::from_secs(2));
                Self::open_with(ep, Duration::from_secs(2), 2, Duration::from_secs(3))
                    .map_err(|e| anyhow!("{first:#}; retry with slower timing: {e:#}"))
            }
        }
    }

    fn open_with(ep: &Endpoint, pre_delay: Duration, attempts: u32, wait: Duration) -> Result<Self> {
        let addr = SocketAddr::from((ep.ip, ep.tcp_port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .with_context(|| format!("connect to {addr}"))?;
        stream.set_nodelay(true)?;
        let mut s = Self { stream, framed: true };
        thread::sleep(pre_delay);
        s.stream.set_read_timeout(Some(wait))?;
        let mut answer = None;
        for _ in 0..attempts {
            s.send(&HELLO, TAG_STCP)?;
            match s.recv(8) {
                Ok(a) => {
                    answer = Some(a);
                    break;
                }
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e),
            }
        }
        let answer = answer.ok_or_else(|| anyhow!("no hello answer from {addr}"))?;
        let flags = u32::from_le_bytes(answer[4..8].try_into()?);
        if flags & 0x400 != 0 {
            bail!("logger refused the session (hello flags 0x{flags:x})");
        }
        if flags & 0x200 != 0 {
            bail!("logger wants a password (hello flags 0x{flags:x}); not supported yet");
        }
        s.framed = flags & 0x100 != 0;
        s.stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        s.command(CMD_IDENTIFY, b"", &mut |_, _| {})?;
        Ok(s)
    }

    fn send(&mut self, data: &[u8], tag: &[u8; 4]) -> Result<()> {
        if self.framed {
            self.stream.write_all(&frame(data, tag))?;
        } else {
            self.stream.write_all(data)?;
        }
        Ok(())
    }

    fn recv(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; if self.framed { n + 20 } else { n }];
        self.stream.read_exact(&mut buf)?;
        if self.framed {
            Ok(unframe(&buf, n)?.to_vec())
        } else {
            Ok(buf)
        }
    }

    /// Run one command; returns the reply data (empty if the command has none).
    pub fn command(&mut self, cmd: u32, params: &[u8], progress: Progress) -> Result<Vec<u8>> {
        if params.len() > 32 {
            bail!("inline params are limited to 32 bytes, got {}", params.len());
        }
        let mut header = [0u8; 64];
        header[0x08..0x0c].copy_from_slice(&cmd.to_le_bytes());
        header[0x18] = 0x01; // reply wanted
        header[0x20..0x20 + params.len()].copy_from_slice(params);
        self.send(&header, TAG_STNC)?;

        let ack = self.recv(64)?;
        if ack[0x18] & 0x24 != 0 {
            return Err(Rejected { cmd, flags: ack[0x18] }.into());
        }
        let reply = self.recv(64)?;
        let reply_cmd = u32::from_le_bytes(reply[0x08..0x0c].try_into()?);
        if reply_cmd != cmd {
            bail!("reply is for cmd 0x{reply_cmd:x}, expected 0x{cmd:x}");
        }
        let len = u32::from_le_bytes(reply[0x10..0x14].try_into()?) as usize;
        let chunk = match u32::from_le_bytes(reply[0x14..0x18].try_into()?) as usize {
            0 => len.max(1),
            c => c,
        };
        let mut data = Vec::with_capacity(len);
        progress(0, len as u64);
        while data.len() < len {
            let offset = data.len() as u32;
            self.send(&offset.to_le_bytes(), TAG_STCP)?;
            let n = chunk.min(len - data.len());
            let pkt = self.recv(n + 4)?;
            let got = u32::from_le_bytes(pkt[0..4].try_into()?);
            if got != offset {
                bail!("data packet for offset {got}, expected {offset}");
            }
            data.extend_from_slice(&pkt[4..]);
            progress(data.len() as u64, len as u64);
        }
        Ok(data)
    }

    pub fn hardware_info(&mut self) -> Result<String> {
        let raw = self.command(CMD_HW_INFO, b"", &mut |_, _| {})?;
        Ok(String::from_utf8_lossy(&raw).trim_end_matches('\0').to_string())
    }

    pub fn list_sessions(&mut self) -> Result<Vec<DeviceSession>> {
        let csv = self.command(CMD_SUMMARY, b"", &mut |_, _| {})?;
        Ok(parse_summary(&String::from_utf8_lossy(&csv)))
    }

    pub fn read_file(&mut self, name: &str, progress: Progress) -> Result<Vec<u8>> {
        if name.contains(['/', '\\', ',']) {
            bail!("unexpected session name {name:?}");
        }
        let path = format!("{RECORDED_DIR}/{name}");
        self.command(CMD_READ_FILE, path.as_bytes(), progress)
            .with_context(|| format!("read {path}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        // The logger keeps reporting itself busy for ~0.5 s after a close.
        thread::sleep(Duration::from_millis(700));
    }
}

fn is_timeout(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|io| matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

// --- session summary (cmd 0x20024) --------------------------------------------------------

/// One row of the logger's recorded-sessions CSV.
#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq)]
pub struct DeviceSession {
    pub name: String,
    pub size: u64,
    /// ISO date (the CSV has dd/mm/yyyy).
    pub date: String,
    pub time: String,
    pub laps: u32,
    pub best_lap: Option<u32>,
    pub best_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub driver: String,
    pub track: String,
    pub vehicle: String,
    pub championship: String,
    /// Why recording stopped (`stop`, `mark`, …).
    pub end_reason: String,
    pub track_lat: Option<f64>,
    pub track_lon: Option<f64>,
}

pub fn parse_summary(csv: &str) -> Vec<DeviceSession> {
    let mut lines = csv.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty());
    let Some(header) = lines.next() else { return vec![] };
    let cols: Vec<&str> = header.split(',').collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);
    let (i_name, i_size, i_date, i_hour) = (idx("name"), idx("size"), idx("date"), idx("hour"));
    let (i_nlap, i_nbest, i_best) = (idx("nlap"), idx("nbest"), idx("best"));
    let (i_pilot, i_track, i_vehicle, i_champ) =
        (idx("pilota"), idx("track_name"), idx("veicolo"), idx("campionato"));
    let (i_reason, i_lat, i_lon, i_dur) =
        (idx("motivolap"), idx("track_lat"), idx("track_lon"), idx("test_dur"));

    lines
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            let get = |i: Option<usize>| i.and_then(|i| f.get(i)).map(|s| s.trim()).unwrap_or("");
            let num = |i: Option<usize>| get(i).parse::<u64>().ok();
            let coord = |i: Option<usize>| get(i).parse::<i64>().ok().map(|v| v as f64 / 1e7);
            let name = get(i_name);
            if name.is_empty() {
                return None;
            }
            Some(DeviceSession {
                name: name.to_string(),
                size: num(i_size).unwrap_or(0),
                date: iso_date(get(i_date)),
                time: get(i_hour).to_string(),
                laps: num(i_nlap).unwrap_or(0) as u32,
                best_lap: num(i_nbest).map(|v| v as u32),
                best_ms: num(i_best),
                duration_ms: num(i_dur),
                driver: get(i_pilot).to_string(),
                track: get(i_track).to_string(),
                vehicle: get(i_vehicle).to_string(),
                championship: get(i_champ).to_string(),
                end_reason: get(i_reason).to_string(),
                track_lat: coord(i_lat),
                track_lon: coord(i_lon),
            })
        })
        .collect()
}

fn iso_date(dmy: &str) -> String {
    match dmy.split('/').collect::<Vec<_>>()[..] {
        [d, m, y] => format!("{y}-{m:0>2}-{d:0>2}"),
        _ => dmy.to_string(),
    }
}

// --- one-shot helpers for the CLI ----------------------------------------------------------

/// Find the logger (keep-alive), falling back to the configured address.
pub fn locate(ep: &Endpoint, ka: &KeepAlive) -> Endpoint {
    let ip = ka.wait_for_device(Duration::from_secs(3)).map(|a| a.ip).unwrap_or(ep.ip);
    Endpoint { ip, ..ep.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_matches_the_wire_capture() {
        let wire = "3c 68 53 54 43 50 08 00 00 00 00 3e 00 00 00 00 06 08 00 00 3c 53 54 43 50 0e 00 3e";
        let bytes: Vec<u8> = wire.split(' ').map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        assert_eq!(frame(&HELLO, TAG_STCP), bytes);
        assert_eq!(unframe(&bytes, 8).unwrap(), &HELLO);
    }

    #[test]
    fn unframe_rejects_bad_checksum() {
        let mut f = frame(b"abc", TAG_STCP);
        let n = f.len();
        f[n - 3] ^= 1;
        assert!(unframe(&f, 3).is_err());
    }

    #[test]
    fn parses_the_solo_keepalive() {
        // ka-reply-00.bin from run 20261012-123400.
        let mut p = vec![0u8; 0xec];
        p[0] = 0xec;
        p[4] = 2;
        p[8..12].copy_from_slice(&[0x0a, 0, 0, 1]);
        p[0x0e] = 0x05;
        p[0x11] = 0x02;
        p[0x5c..0x60].copy_from_slice(&510u32.to_le_bytes());
        let a = parse_keepalive(&p, Ipv4Addr::new(10, 0, 0, 1)).unwrap();
        assert_eq!((a.logger_code, a.busy, a.owner), (510, true, 2));
        assert!(parse_keepalive(&p[..100], Ipv4Addr::LOCALHOST).is_none());
    }

    #[test]
    fn parses_the_session_summary() {
        let csv = "name,size,date,hour,nlap,nbest,best,pilota,track_name,veicolo,campionato,venue_type,mode,trk_type,motivolap,maxvel,device,track_lat,track_lon,test_dur,ptype,ptime,pdist,pmaxv,\r\n\
                   a_0126.xrz,1593162,07/09/2026,13:43:51,12,5,74796,,Great Bend T,,,,speed,closed,stop,-2063597568,,432888035,-817180414,1074326,,,,,\r\n\
                   a_0123.xrz,1160,07/09/2026,12:37:09,0,,,,Great Bend T,,,,speed,closed,mark,-2097152000,,432888035,-817180414,16162,,,,,\r\n";
        let rows = parse_summary(csv);
        assert_eq!(rows.len(), 2);
        let r = &rows[0];
        assert_eq!((r.name.as_str(), r.size, r.date.as_str(), r.laps), ("a_0126.xrz", 1593162, "2026-09-07", 12));
        assert_eq!((r.best_lap, r.best_ms, r.duration_ms), (Some(5), Some(74796), Some(1074326)));
        assert_eq!(r.track, "Great Bend T");
        assert!((r.track_lat.unwrap() - 43.2888035).abs() < 1e-9);
        assert_eq!((rows[1].best_ms, rows[1].end_reason.as_str()), (None, "mark"));
    }
}
