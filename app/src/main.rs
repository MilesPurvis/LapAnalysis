//! LapAnalysis: download AiM Solo 2 DL sessions over Wi-Fi and browse them in a local web app.

mod aim;
mod fake;
mod library;
mod web;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use aim::{Endpoint, KeepAlive, Session};
use library::Library;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Where downloaded sessions are kept.
    #[arg(long, global = true, env = "LAPANALYSIS_LIBRARY")]
    library: Option<PathBuf>,
    /// Logger address if discovery doesn't find it (the Solo's own Wi-Fi uses 10.0.0.1).
    #[arg(long, global = true, default_value_t = aim::DEFAULT_IP)]
    solo_ip: Ipv4Addr,
    #[arg(long, global = true, default_value_t = aim::TCP_PORT, hide = true)]
    solo_tcp_port: u16,
    #[arg(long, global = true, default_value_t = aim::UDP_PORT, hide = true)]
    solo_udp_port: u16,
    /// Local UDP port for discovery; RS3 uses the same 36002 on both ends.
    #[arg(long, global = true, default_value_t = aim::UDP_PORT, hide = true)]
    udp_local_port: u16,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web app (default).
    Serve {
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// List the sessions on the logger.
    List,
    /// Download sessions into the library: the named ones, or every one not there yet.
    Pull { names: Vec<String> },
    /// Print a downloaded file's laps and channels.
    Info { file: PathBuf },
    /// Serve a fake logger from a folder of .xrz files (for UI work away from the Solo).
    #[command(hide = true)]
    FakeSolo {
        dir: PathBuf,
        /// Summary CSV to serve; defaults to one generated from the folder.
        #[arg(long)]
        csv: Option<PathBuf>,
        #[arg(long, default_value_t = 2001)]
        tcp_port: u16,
        #[arg(long, default_value_t = 36012)]
        udp_port: u16,
    },
}

/// `<repo>/library`, wherever the checkout lives (git-ignored; created on first run).
fn default_library() -> PathBuf {
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    app_dir.parent().unwrap_or(app_dir).join("library")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ep = Endpoint { ip: cli.solo_ip, tcp_port: cli.solo_tcp_port, udp_port: cli.solo_udp_port };
    let lib_dir = cli.library.clone().unwrap_or_else(default_library);
    match cli.command.unwrap_or(Command::Serve { port: 8080 }) {
        Command::Serve { port } => serve(Library::open(lib_dir)?, ep, cli.udp_local_port, port),
        Command::List => {
            let ka = KeepAlive::start(&ep, cli.udp_local_port);
            let ep = aim::locate(&ep, &ka);
            let mut s = Session::open(&ep)?;
            println!("logger at {} · {}", ep.ip, s.hardware_info().unwrap_or_default());
            for r in s.list_sessions()? {
                let best = r.best_ms.map(fmt_lap).unwrap_or_default();
                println!("{:<12} {:>9} B  {} {}  {:>2} laps {:>9}  {}", r.name, r.size, r.date, r.time, r.laps, best, r.track);
            }
            Ok(())
        }
        Command::Pull { names } => pull(&Library::open(lib_dir)?, &ep, cli.udp_local_port, names),
        Command::Info { file } => info(&file),
        Command::FakeSolo { dir, csv, tcp_port, udp_port } => fake_solo(&dir, csv, tcp_port, udp_port),
    }
}

#[tokio::main]
async fn serve(lib: Library, ep: Endpoint, udp_local: u16, port: u16) -> Result<()> {
    let ka = KeepAlive::start(&ep, udp_local);
    println!("library: {} ({} downloaded sessions)", lib.dir().display(), lib.file_names()?.len());
    let app = web::App::new(lib, ka, ep);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;
    println!("LapAnalysis v{} running at http://{addr}  (Ctrl-C to stop)", env!("CARGO_PKG_VERSION"));
    axum::serve(listener, web::router(app)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}

fn pull(lib: &Library, ep: &Endpoint, udp_local: u16, names: Vec<String>) -> Result<()> {
    let ka = KeepAlive::start(ep, udp_local);
    let ep = aim::locate(ep, &ka);
    let mut s = Session::open(&ep)?;
    let listing = s.list_sessions()?;
    let wanted: Vec<_> = if names.is_empty() {
        listing.iter().filter(|r| !lib.contains(&r.name)).cloned().collect()
    } else {
        names
            .iter()
            .map(|n| listing.iter().find(|r| &r.name == n).cloned().with_context(|| format!("{n} isn't on the logger")))
            .collect::<Result<_>>()?
    };
    if wanted.is_empty() {
        println!("library is up to date ({} sessions on the logger)", listing.len());
        return Ok(());
    }
    let mut failed = 0;
    for r in &wanted {
        let started = Instant::now();
        let mut last_print = Instant::now() - Duration::from_secs(1);
        let result = s.read_file(&r.name, &mut |got, total| {
            if last_print.elapsed() > Duration::from_millis(500) || got == total {
                eprint!("\r{:<12} {:>3}%  {got}/{total} B", r.name, (got * 100).checked_div(total).unwrap_or(100));
                last_print = Instant::now();
            }
        });
        eprintln!();
        match result.and_then(|data| {
            if data.len() as u64 != r.size {
                bail!("got {} bytes, the logger listed {}", data.len(), r.size);
            }
            lib.store(r, &data)
        }) {
            Ok(()) => {
                let secs = started.elapsed().as_secs_f64();
                println!("{:<12} saved  {} B in {secs:.1}s ({:.0} KiB/s)", r.name, r.size, r.size as f64 / secs / 1024.0);
            }
            Err(e) => {
                failed += 1;
                println!("{:<12} FAILED {e:#}", r.name);
                s = Session::open(&ep)?;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} downloads failed", wanted.len());
    }
    Ok(())
}

fn info(file: &Path) -> Result<()> {
    let dir = file.parent().context("file has no folder")?;
    let name = file.file_name().context("not a file")?.to_string_lossy().to_string();
    let lib = Library::open(dir)?;
    let d = lib.detail(&name)?;
    println!("{} · {} · {} {} · {} · {:.1}s", d.name, d.kind, d.date, d.time, d.venue, d.duration_s);
    if let Some(e) = &d.error {
        println!("error: {e}");
    }
    for l in &d.laps {
        println!("  lap {:>2} {:<4} {}{}", l.num, l.kind, fmt_lap(l.duration_ms as u64), if l.best { "  best" } else { "" });
    }
    for c in &d.channels {
        println!("  {:<6} {:<20} {:<6} {:>6} samples {:>6.1} Hz", c.id, c.name, c.units, c.samples, c.rate_hz);
    }
    Ok(())
}

fn fake_solo(dir: &Path, csv: Option<PathBuf>, tcp_port: u16, udp_port: u16) -> Result<()> {
    let mut files = HashMap::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xrz")) {
            files.insert(p.file_name().unwrap().to_string_lossy().to_string(), std::fs::read(&p)?);
        }
    }
    let csv = match csv {
        Some(p) => std::fs::read_to_string(p)?,
        None => {
            let mut s = String::from("name,size,date,hour,nlap,nbest,best,pilota,track_name\r\n");
            for (n, d) in &files {
                s += &format!("{n},{},07/09/2026,12:00:00,0,,,,Fake Track\r\n", d.len());
            }
            s
        }
    };
    let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, tcp_port))?;
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, udp_port))?;
    let f = fake::spawn(tcp, Some(udp), fake::FakeConfig { csv, files, chunk: 65472 });
    println!("fake Solo on tcp {} / udp {} — run: lapanalysis --solo-ip 127.0.0.1 --solo-tcp-port {tcp_port} --solo-udp-port {udp_port} --udp-local-port 0 serve", f.tcp, f.udp.unwrap());
    loop {
        std::thread::park();
    }
}

fn fmt_lap(ms: u64) -> String {
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(files: &[(&str, Vec<u8>)], chunk: u32) -> (Endpoint, KeepAlive) {
        let mut csv = String::from("name,size,date,hour,nlap,nbest,best,pilota,track_name\r\n");
        for (n, d) in files {
            csv += &format!("{n},{},07/09/2026,14:08:51,1,,,,Great Bend T\r\n", d.len());
        }
        let cfg = fake::FakeConfig { csv, files: files.iter().map(|(n, d)| (n.to_string(), d.clone())).collect(), chunk };
        let f = fake::spawn(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            Some(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()),
            cfg,
        );
        let ep = Endpoint { ip: Ipv4Addr::LOCALHOST, tcp_port: f.tcp.port(), udp_port: f.udp.unwrap().port() };
        let ka = KeepAlive::start(&ep, 0);
        (ep, ka)
    }

    #[test]
    fn lists_and_downloads_multi_chunk_files() {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let (ep, _ka) = fake(&[("a_0001.xrz", big.clone()), ("a_0002.xrz", b"tiny".to_vec())], 65472);
        let mut s = Session::open(&ep).unwrap();
        assert_eq!(s.hardware_info().unwrap(), "WiFi=ESP32|Reg=usa|Rev=02|");
        let rows = s.list_sessions().unwrap();
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["a_0001.xrz", "a_0002.xrz"]);
        let mut calls = 0;
        let got = s.read_file("a_0001.xrz", &mut |_, _| calls += 1).unwrap();
        assert_eq!(got, big);
        assert_eq!(calls, 1 + 5, "one progress call up front, then one per chunk");
        assert_eq!(s.read_file("a_0002.xrz", &mut |_, _| {}).unwrap(), b"tiny");
    }

    #[test]
    fn rejected_commands_surface_as_errors_and_the_session_survives() {
        let (ep, _ka) = fake(&[("a_0001.xrz", vec![1, 2, 3])], 65472);
        let mut s = Session::open(&ep).unwrap();
        let err = s.command(0x20005, b"", &mut |_, _| {}).unwrap_err();
        assert!(err.downcast_ref::<aim::Rejected>().is_some(), "{err:#}");
        assert_eq!(s.read_file("a_0001.xrz", &mut |_, _| {}).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn keepalive_discovers_the_logger() {
        let (_ep, ka) = fake(&[], 65472);
        let a = ka.wait_for_device(Duration::from_secs(3)).expect("no keep-alive answer");
        assert_eq!((a.ip, a.logger_code, a.busy), (Ipv4Addr::LOCALHOST, 510, false));
    }

    #[test]
    fn pull_stores_new_sessions_once() {
        let dir = std::env::temp_dir().join(format!("lapanalysis-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (ep, _ka) = fake(&[("a_0001.xrz", vec![9; 100_000])], 40_000);
        let lib = Library::open(&dir).unwrap();
        pull(&lib, &ep, 0, vec![]).unwrap();
        assert_eq!(std::fs::read(dir.join("a_0001.xrz")).unwrap(), vec![9; 100_000]);
        assert!(dir.join("index.json").is_file());
        pull(&lib, &ep, 0, vec![]).unwrap(); // nothing new: no error, no re-download
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
