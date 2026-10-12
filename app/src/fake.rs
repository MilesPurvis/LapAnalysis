//! A fake Solo 2 DL speaking the same protocol, for tests and UI work away from the logger.
//! It mirrors what the real device sent in runs 20261012-125645 … -132551 (LOG.md).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crate::aim::frame;

pub struct FakeConfig {
    /// Summary CSV returned by cmd 0x20024.
    pub csv: String,
    /// Files served by cmd 0x40002 "1:/mem/<name>".
    pub files: HashMap<String, Vec<u8>>,
    /// Data chunk size advertised in reply headers (the Solo uses 65472).
    pub chunk: u32,
}

pub struct FakeSolo {
    pub tcp: SocketAddr,
    pub udp: Option<SocketAddr>,
}

/// Start serving on the given listeners in background threads.
pub fn spawn(tcp: TcpListener, udp: Option<UdpSocket>, cfg: FakeConfig) -> FakeSolo {
    let cfg = Arc::new(cfg);
    let connected = Arc::new(AtomicBool::new(false));
    let fake = FakeSolo { tcp: tcp.local_addr().unwrap(), udp: udp.as_ref().map(|u| u.local_addr().unwrap()) };
    if let Some(udp) = udp {
        let connected = connected.clone();
        thread::spawn(move || keepalive_responder(udp, connected));
    }
    thread::spawn(move || {
        for stream in tcp.incoming().flatten() {
            let (cfg, connected) = (cfg.clone(), connected.clone());
            thread::spawn(move || {
                connected.store(true, Ordering::Relaxed);
                let _ = serve(stream, &cfg);
                connected.store(false, Ordering::Relaxed);
            });
        }
    });
    fake
}

fn keepalive_responder(udp: UdpSocket, connected: Arc<AtomicBool>) {
    let mut buf = [0u8; 64];
    while let Ok((n, from)) = udp.recv_from(&mut buf) {
        if &buf[..n] != b"aim-ka" {
            continue;
        }
        let mut p = vec![0u8; 0xec];
        p[0] = 0xec;
        p[4] = 2;
        p[8..12].copy_from_slice(&[127, 0, 0, 1]);
        p[0x0e] = 0x04 | connected.load(Ordering::Relaxed) as u8;
        p[0x11] = if connected.load(Ordering::Relaxed) { 2 } else { 0 };
        p[0x5c..0x60].copy_from_slice(&510u32.to_le_bytes());
        let _ = udp.send_to(&p, from);
    }
}

fn rx_frame(s: &mut TcpStream, n: usize, tag: &[u8; 4]) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n + 20];
    s.read_exact(&mut buf)?;
    let ok = &buf[..2] == b"<h" && &buf[2..6] == tag && u32::from_le_bytes(buf[6..10].try_into().unwrap()) as usize == n;
    if !ok {
        return Err(std::io::Error::other(format!("fake: bad frame {:02x?}", &buf[..12])));
    }
    Ok(buf[12..12 + n].to_vec())
}

fn serve(mut s: TcpStream, cfg: &FakeConfig) -> std::io::Result<()> {
    rx_frame(&mut s, 8, b"STCP")?;
    let mut answer = [0u8; 8];
    answer[4..8].copy_from_slice(&0x906u32.to_le_bytes());
    s.write_all(&frame(&answer, b"STCP"))?;
    loop {
        let h = rx_frame(&mut s, 64, b"STNC")?;
        let cmd = u32::from_le_bytes(h[8..12].try_into().unwrap());
        let params = String::from_utf8_lossy(&h[0x20..]).trim_end_matches('\0').to_string();
        let data: Option<Vec<u8>> = match cmd {
            0x10002 => Some(vec![]),
            0x10008 => Some(b"WiFi=ESP32|Reg=usa|Rev=02|\0".to_vec()),
            0x20024 => Some(cfg.csv.clone().into_bytes()),
            0x40002 => Some(
                params.strip_prefix("1:/mem/").and_then(|n| cfg.files.get(n)).cloned().unwrap_or_default(),
            ),
            _ => None,
        };
        let mut ack = [0u8; 64];
        ack[8..12].copy_from_slice(&cmd.to_le_bytes());
        ack[0x20..].copy_from_slice(&h[0x20..]);
        let Some(data) = data else {
            ack[0x18] = 0x05; // invalid command, as the Solo answers 0x20005
            s.write_all(&frame(&ack, b"STCP"))?;
            continue;
        };
        ack[0x14..0x18].copy_from_slice(&cfg.chunk.to_le_bytes());
        ack[0x18] = 0x09;
        s.write_all(&frame(&ack, b"STCP"))?;
        let mut reply = ack;
        reply[0x10..0x14].copy_from_slice(&(data.len() as u32).to_le_bytes());
        reply[0x18] = 0x11;
        s.write_all(&frame(&reply, b"STCP"))?;
        let mut sent = 0usize;
        while sent < data.len() {
            let off = u32::from_le_bytes(rx_frame(&mut s, 4, b"STCP")?.try_into().unwrap()) as usize;
            let end = (off + cfg.chunk as usize).min(data.len());
            let mut pkt = (off as u32).to_le_bytes().to_vec();
            pkt.extend_from_slice(&data[off..end]);
            s.write_all(&frame(&pkt, b"STCP"))?;
            sent = end;
        }
    }
}
