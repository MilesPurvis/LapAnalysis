//! Local web UI: JSON API over the device client and the library, plus embedded assets.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::aim::{DeviceSession, DeviceStatus, Endpoint, KeepAlive, Session};
use crate::info::SessionInfo;
use crate::library::Library;

pub struct App {
    pub lib: Library,
    pub ka: KeepAlive,
    pub ep: Endpoint,
    /// The logger handles one TCP session at a time.
    device: Mutex<()>,
    listing: Mutex<Option<Vec<DeviceSession>>>,
    job: Mutex<Job>,
}

#[derive(Clone, Default, Serialize)]
struct Job {
    running: bool,
    queue: Vec<String>,
    current: Option<String>,
    file_bytes: u64,
    file_total: u64,
    done: Vec<String>,
    failed: Vec<Failure>,
    finished_at: Option<u64>,
}

#[derive(Clone, Serialize)]
struct Failure {
    name: String,
    error: String,
}

impl App {
    pub fn new(lib: Library, ka: KeepAlive, ep: Endpoint) -> Arc<Self> {
        Arc::new(Self { lib, ka, ep, device: Mutex::new(()), listing: Mutex::new(None), job: Mutex::new(Job::default()) })
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint { ip: self.ka.found_ip().unwrap_or(self.ep.ip), ..self.ep.clone() }
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { index() }))
        .route("/assets/{file}", get(|Path(file): Path<String>| async move { asset(&file) }))
        .route("/api/device", get(device_status))
        .route("/api/device/sessions", get(device_sessions))
        .route("/api/device/download", get(download_status).post(download_start))
        .route("/api/library", get(library_list))
        .route("/api/laps", get(lap_catalog))
        .route("/api/compare", get(compare_laps))
        .route("/api/library/{name}", get(library_detail))
        .route("/api/library/{name}/info", put(library_update_info))
        .route("/api/library/{name}/series", get(library_series))
        .route("/api/library/{name}/track", get(library_track))
        .route("/api/library/{name}/raw", get(library_raw))
        .with_state(app)
}

fn index() -> Response {
    let html = include_str!("../static/index.html").replace("__VERSION__", env!("CARGO_PKG_VERSION"));
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], html).into_response()
}

fn asset(name: &str) -> Response {
    let (body, mime): (&'static [u8], &str) = match name {
        "app.js" => (include_bytes!("../static/app.js"), "text/javascript; charset=utf-8"),
        "style.css" => (include_bytes!("../static/style.css"), "text/css; charset=utf-8"),
        "uPlot.iife.min.js" => (include_bytes!("../static/uPlot.iife.min.js"), "text/javascript; charset=utf-8"),
        "uPlot.min.css" => (include_bytes!("../static/uPlot.min.css"), "text/css; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

struct ApiError(StatusCode, anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": format!("{:#}", self.1) }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow!("task failed: {e}"))?
}

// --- device ------------------------------------------------------------------------------

#[derive(Serialize)]
struct DeviceInfo {
    #[serde(flatten)]
    status: DeviceStatus,
    address: String,
    busy_with_download: bool,
}

async fn device_status(State(app): State<Arc<App>>) -> Json<DeviceInfo> {
    let ep = app.endpoint();
    Json(DeviceInfo {
        status: app.ka.status(),
        address: format!("{}:{}", ep.ip, ep.tcp_port),
        busy_with_download: app.job.lock().unwrap().running,
    })
}

#[derive(Serialize)]
struct DeviceListing {
    sessions: Vec<ListedSession>,
}

#[derive(Serialize)]
struct ListedSession {
    #[serde(flatten)]
    session: DeviceSession,
    in_library: bool,
}

async fn device_sessions(State(app): State<Arc<App>>) -> ApiResult<DeviceListing> {
    if app.job.lock().unwrap().running {
        return Err(ApiError(StatusCode::CONFLICT, anyhow!("a download is running")));
    }
    let a = app.clone();
    let sessions = blocking(move || {
        let _guard = a.device.lock().unwrap();
        a.ka.touch();
        let mut s = Session::open(&a.endpoint())?;
        s.list_sessions()
    })
    .await
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    *app.listing.lock().unwrap() = Some(sessions.clone());
    let sessions = sessions
        .into_iter()
        .map(|s| ListedSession { in_library: app.lib.contains(&s.name), session: s })
        .collect();
    Ok(Json(DeviceListing { sessions }))
}

#[derive(Deserialize)]
struct DownloadRequest {
    names: Vec<String>,
}

async fn download_status(State(app): State<Arc<App>>) -> Json<Job> {
    app.ka.touch();
    Json(app.job.lock().unwrap().clone())
}

async fn download_start(State(app): State<Arc<App>>, Json(req): Json<DownloadRequest>) -> ApiResult<Job> {
    let rows: Vec<DeviceSession> = {
        let listing = app.listing.lock().unwrap();
        let listing = listing.as_ref().ok_or_else(|| {
            ApiError(StatusCode::BAD_REQUEST, anyhow!("load the device's session list first"))
        })?;
        req.names
            .iter()
            .map(|n| listing.iter().find(|s| &s.name == n).cloned().with_context(|| format!("{n} isn't on the device")))
            .collect::<anyhow::Result<_>>()
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?
    };
    {
        let mut job = app.job.lock().unwrap();
        if job.running {
            return Err(ApiError(StatusCode::CONFLICT, anyhow!("a download is already running")));
        }
        *job = Job { running: true, queue: rows.iter().map(|r| r.name.clone()).collect(), ..Job::default() };
    }
    let a = app.clone();
    std::thread::spawn(move || run_downloads(&a, rows));
    Ok(Json(app.job.lock().unwrap().clone()))
}

fn run_downloads(app: &App, rows: Vec<DeviceSession>) {
    let _guard = app.device.lock().unwrap();
    let mut session: Option<Session> = None;
    for row in rows {
        {
            let mut job = app.job.lock().unwrap();
            job.queue.retain(|n| n != &row.name);
            job.current = Some(row.name.clone());
            (job.file_bytes, job.file_total) = (0, row.size);
        }
        app.ka.touch();
        let result = (|| -> anyhow::Result<()> {
            if session.is_none() {
                session = Some(Session::open(&app.endpoint())?);
            }
            let s = session.as_mut().unwrap();
            let data = s.read_file(&row.name, &mut |got, total| {
                let mut job = app.job.lock().unwrap();
                (job.file_bytes, job.file_total) = (got, total);
            })?;
            if data.len() as u64 != row.size {
                anyhow::bail!("got {} bytes, the logger listed {}", data.len(), row.size);
            }
            app.lib.store(&row, &data)
        })();
        let mut job = app.job.lock().unwrap();
        match result {
            Ok(()) => job.done.push(row.name),
            Err(e) => {
                session = None; // the stream may be mid-transfer; start clean
                job.failed.push(Failure { name: row.name, error: format!("{e:#}") });
            }
        }
    }
    drop(session);
    let mut job = app.job.lock().unwrap();
    job.running = false;
    job.current = None;
    job.finished_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
}

// --- library -----------------------------------------------------------------------------

async fn library_list(State(app): State<Arc<App>>) -> ApiResult<Vec<crate::library::LibraryItem>> {
    let a = app.clone();
    Ok(Json(blocking(move || a.lib.list()).await?))
}

async fn library_detail(State(app): State<Arc<App>>, Path(name): Path<String>) -> ApiResult<crate::library::SessionDetail> {
    let a = app.clone();
    blocking(move || a.lib.detail(&name)).await.map(Json).map_err(|e| ApiError(StatusCode::NOT_FOUND, e))
}

async fn lap_catalog(State(app): State<Arc<App>>) -> ApiResult<Vec<crate::compare::SessionLaps>> {
    let a = app.clone();
    Ok(Json(blocking(move || crate::compare::catalog(&a.lib)).await?))
}

#[derive(Deserialize)]
struct CompareQuery {
    /// `file:lap` pairs, reference first: `a_0124.xrz:3,a_0126.xrz:4`.
    laps: String,
    /// Channel names, comma-separated.
    #[serde(default)]
    channels: String,
    points: Option<usize>,
}

async fn compare_laps(State(app): State<Arc<App>>, Query(q): Query<CompareQuery>) -> ApiResult<crate::compare::Comparison> {
    let wanted = q
        .laps
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (file, lap) = s.rsplit_once(':').with_context(|| format!("bad lap {s:?}, expected file:lap"))?;
            Ok((file.to_string(), lap.parse::<i32>().with_context(|| format!("bad lap number in {s:?}"))?))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let channels: Vec<String> = q.channels.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let a = app.clone();
    blocking(move || crate::compare::compare(&a.lib, &wanted, &channels, q.points.unwrap_or(1200)))
        .await
        .map(Json)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
struct InfoUpdate {
    info: SessionInfo,
    /// Also fill driver/car/track on other sessions from the same day that have none.
    #[serde(default)]
    fill_day: bool,
}

#[derive(Serialize)]
struct InfoSaved {
    filled: usize,
    detail: crate::library::SessionDetail,
}

async fn library_update_info(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Json(req): Json<InfoUpdate>,
) -> ApiResult<InfoSaved> {
    let a = app.clone();
    blocking(move || {
        let filled = a.lib.update_info(&name, req.info, req.fill_day)?;
        Ok(InfoSaved { filled, detail: a.lib.detail(&name)? })
    })
    .await
    .map(Json)
    .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
struct SeriesQuery {
    id: String,
    lap: Option<i32>,
    points: Option<usize>,
}

async fn library_series(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Query(q): Query<SeriesQuery>,
) -> ApiResult<crate::library::Series> {
    let a = app.clone();
    let points = q.points.unwrap_or(1500).clamp(10, 20_000);
    Ok(Json(blocking(move || a.lib.series(&name, &q.id, q.lap, points)).await?))
}

#[derive(Deserialize)]
struct TrackQuery {
    lap: Option<i32>,
    points: Option<usize>,
}

async fn library_track(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    Query(q): Query<TrackQuery>,
) -> ApiResult<crate::library::Track> {
    let a = app.clone();
    let points = q.points.unwrap_or(3000).clamp(10, 50_000);
    Ok(Json(blocking(move || a.lib.track(&name, q.lap, points)).await?))
}

/// `attachment` with an ASCII fallback name plus the UTF-8 one (RFC 6266), since session
/// details can contain any characters.
fn content_disposition(file_name: &str) -> String {
    let ascii: String = file_name
        .chars()
        .filter(|c| *c != '"' && *c != '\\')
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' })
        .collect();
    let encoded: String = file_name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

async fn library_raw(State(app): State<Arc<App>>, Path(name): Path<String>) -> Result<Response, ApiError> {
    let a = app.clone();
    let (data, file_name) = blocking(move || Ok((a.lib.raw(&name)?, a.lib.download_name(&name))))
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    let disposition = content_disposition(&file_name);
    Ok(([(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_DISPOSITION, disposition)], data)
        .into_response())
}
