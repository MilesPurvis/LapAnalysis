"use strict";

// ---------------------------------------------------------------------------------------
// helpers

const $ = (sel, el = document) => el.querySelector(sel);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

async function api(path, opts) {
  const r = await fetch(path, opts);
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || `${r.status} ${r.statusText}`);
  return body;
}

const fmt = {
  lap(ms) {
    if (ms == null) return "–";
    const m = Math.floor(ms / 60000);
    const s = ((ms % 60000) / 1000).toFixed(3).padStart(6, "0");
    return `${m}:${s}`;
  },
  dur(ms) {
    if (ms == null) return "–";
    const t = Math.round(ms / 1000), h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
    const ss = String(s).padStart(2, "0");
    return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
  },
  clock(sec, decimals = 0) {
    const neg = sec < 0 ? "-" : "";
    sec = Math.abs(sec);
    const s = (sec % 60).toFixed(decimals).padStart(decimals ? 3 + decimals : 2, "0");
    return `${neg}${Math.floor(sec / 60)}:${s}`;
  },
  bytes(n) {
    if (n == null) return "–";
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
  },
  date(iso) {
    if (!iso) return "–";
    const d = new Date(`${iso}T00:00:00`);
    return isNaN(d) ? iso : d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
  },
  num(v, units) {
    if (v == null || !isFinite(v)) return "–";
    const a = Math.abs(v), d = a >= 100 ? 0 : a >= 10 ? 1 : 2;
    return `${v.toFixed(d)}${units ? " " + units : ""}`;
  },
};

const cssVar = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
const store = {
  get(k, d) { try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* private mode */ } },
};

const PALETTE = ["#e5484d", "#3b82f6", "#22c55e", "#f59e0b", "#a855f7", "#14b8a6", "#ec4899", "#64748b", "#84cc16", "#0ea5e9"];

// ---------------------------------------------------------------------------------------
// device status (header pill, shared with the logger view)

const device = { status: null, listeners: new Set() };

async function pollDevice() {
  try {
    device.status = await api("/api/device");
  } catch {
    device.status = null;
  }
  const pill = $("#device-pill"), s = device.status;
  pill.classList.toggle("online", !!s?.online);
  pill.classList.toggle("busy", !!(s?.busy_with_download));
  let label = "Logger not found";
  if (s?.busy_with_download) label = "Downloading…";
  else if (s?.online) label = `Solo · ${s.last.ip}`;
  else if (s?.discovery_error) label = "Discovery off";
  $(".label", pill).textContent = label;
  device.listeners.forEach((fn) => fn(s));
}
setInterval(pollDevice, 2000);
pollDevice();

// ---------------------------------------------------------------------------------------
// router

let cleanup = null;

function route() {
  if (cleanup) { cleanup(); cleanup = null; }
  const hash = location.hash || "#/library";
  const [, page, arg] = hash.match(/^#\/([^/]+)\/?(.*)$/) || [, "library"];
  document.querySelectorAll("nav a").forEach((a) => a.classList.toggle("active", a.dataset.nav === (page === "session" ? "library" : page)));
  const view = $("#view");
  if (page === "logger") cleanup = loggerView(view);
  else if (page === "session" && arg) cleanup = sessionView(view, decodeURIComponent(arg));
  else cleanup = libraryView(view);
  window.scrollTo(0, 0);
}
window.addEventListener("hashchange", route);

// ---------------------------------------------------------------------------------------
// library

function libraryView(view) {
  view.innerHTML = `
    <h1>Library</h1>
    <p class="sub">Sessions downloaded from the logger.</p>
    <div class="card" id="lib"><div class="empty">Loading…</div></div>`;
  let alive = true;
  api("/api/library").then((items) => {
    if (!alive) return;
    const el = $("#lib");
    if (!items.length) {
      el.innerHTML = `<div class="empty"><strong>No sessions yet</strong>Join the Solo's Wi-Fi and download them from the <a href="#/logger">Logger</a> tab.</div>`;
      return;
    }
    el.innerHTML = `<div class="table-wrap"><table>
      <thead><tr><th>Session</th><th>Date</th><th>Time</th><th>Track</th><th class="num">Laps</th><th class="num">Best lap</th><th class="num">Duration</th><th class="num">Size</th></tr></thead>
      <tbody>${items.map((i) => `
        <tr class="link" data-name="${esc(i.name)}">
          <td><span class="mono">${esc(i.name)}</span> ${i.kind !== "session" ? `<span class="chip warn">${i.kind === "log" ? "logger log" : "unreadable"}</span>` : ""}${i.repaired ? ` <span class="chip" title="The logger wrote debug-log lines into this file; they're removed before reading">repaired</span>` : ""}</td>
          <td>${fmt.date(i.date)}</td><td>${esc(i.time) || "–"}</td><td>${esc(i.track) || "–"}</td>
          <td class="num">${i.kind === "session" ? i.laps : "–"}</td>
          <td class="num">${fmt.lap(i.best_ms)}</td>
          <td class="num">${fmt.dur(i.duration_ms)}</td>
          <td class="num">${fmt.bytes(i.size)}</td>
        </tr>`).join("")}</tbody></table></div>`;
    el.querySelectorAll("tr.link").forEach((tr) => tr.addEventListener("click", () => {
      location.hash = `#/session/${encodeURIComponent(tr.dataset.name)}`;
    }));
  }).catch((e) => { $("#lib").innerHTML = `<div class="empty error">${esc(e.message)}</div>`; });
  return () => { alive = false; };
}

// ---------------------------------------------------------------------------------------
// logger

function loggerView(view) {
  view.innerHTML = `
    <h1>Logger</h1>
    <p class="sub">Join the Solo's Wi-Fi network from this Mac, then download its sessions.</p>
    <div class="card" id="status"></div>
    <div class="card">
      <div class="row">
        <h2 style="margin:0">Sessions on the logger</h2>
        <span class="spacer"></span>
        <button id="load">Load sessions</button>
        <button id="dl-sel" disabled>Download selected</button>
        <button id="dl-new" class="primary" disabled>Download all new</button>
      </div>
      <div id="job"></div>
      <div id="list" class="table-wrap"><div class="empty">Sessions appear here once the logger is found.</div></div>
    </div>`;

  let sessions = null, loading = false, polling = null, alive = true;

  function renderStatus(s) {
    const el = $("#status");
    if (!el) return;
    if (s?.online) {
      const owner = s.last.owner ? `busy with .${s.last.owner}` : "idle";
      el.innerHTML = `<div class="row">
        <span class="chip good">Connected</span>
        <span>Solo 2 DL at <span class="mono">${esc(s.address)}</span></span>
        <span class="muted">· logger code ${s.last.logger_code} · ${owner}</span></div>`;
      if (!sessions && !loading && !s.busy_with_download) load();
    } else if (s?.discovery_error) {
      el.innerHTML = `<div class="row"><span class="chip warn">Discovery off</span><span>${esc(s.discovery_error)}. Trying <span class="mono">${esc(s.address)}</span> directly.</span></div>`;
    } else {
      el.innerHTML = `<div class="empty" style="padding:16px"><strong>Logger not found</strong>
        On the Solo, turn Wi-Fi on, then join its network from this Mac. This page finds it automatically.</div>`;
    }
  }

  function renderList() {
    const el = $("#list");
    if (!sessions) return;
    if (!sessions.length) { el.innerHTML = `<div class="empty">The logger has no recorded sessions.</div>`; return; }
    el.innerHTML = `<table>
      <thead><tr><th><input type="checkbox" id="all"></th><th>Session</th><th>Date</th><th>Time</th><th>Track</th><th class="num">Laps</th><th class="num">Best lap</th><th class="num">Duration</th><th class="num">Size</th><th></th></tr></thead>
      <tbody>${sessions.map((s) => `
        <tr>
          <td><input type="checkbox" class="pick" value="${esc(s.name)}"></td>
          <td class="mono">${esc(s.name)}</td>
          <td>${fmt.date(s.date)}</td><td>${esc(s.time)}</td><td>${esc(s.track) || "–"}</td>
          <td class="num">${s.laps}</td><td class="num">${fmt.lap(s.best_ms)}</td>
          <td class="num">${fmt.dur(s.duration_ms)}</td><td class="num">${fmt.bytes(s.size)}</td>
          <td>${s.in_library ? `<a class="chip good" href="#/session/${encodeURIComponent(s.name)}">In library</a>` : `<span class="chip accent">New</span>`}</td>
        </tr>`).join("")}</tbody></table>`;
    const picks = [...el.querySelectorAll(".pick")];
    const sync = () => {
      const n = picks.filter((p) => p.checked).length;
      $("#dl-sel").disabled = !n;
      $("#dl-sel").textContent = n ? `Download selected (${n})` : "Download selected";
    };
    picks.forEach((p) => p.addEventListener("change", sync));
    $("#all").addEventListener("change", (e) => { picks.forEach((p) => (p.checked = e.target.checked)); sync(); });
    const fresh = sessions.filter((s) => !s.in_library).length;
    $("#dl-new").disabled = !fresh;
    $("#dl-new").textContent = fresh ? `Download all new (${fresh})` : "All downloaded";
    sync();
  }

  async function load() {
    loading = true;
    $("#load").disabled = true;
    $("#list").innerHTML = `<div class="empty">Asking the logger…</div>`;
    try {
      sessions = (await api("/api/device/sessions")).sessions;
      if (alive) renderList();
    } catch (e) {
      if (alive) $("#list").innerHTML = `<div class="empty error">${esc(e.message)}</div>`;
    } finally {
      loading = false;
      if (alive) $("#load").disabled = false;
    }
  }

  function renderJob(j) {
    const el = $("#job");
    if (!el) return;
    if (!j.running && !j.done.length && !j.failed.length) { el.innerHTML = ""; return; }
    const pct = j.file_total ? Math.round((j.file_bytes / j.file_total) * 100) : 0;
    const failed = j.failed.map((f) => `<div class="error">✕ ${esc(f.name)}: ${esc(f.error)}</div>`).join("");
    el.innerHTML = `<div class="card" style="margin:14px 0 4px;background:var(--surface-2);box-shadow:none">
      ${j.running ? `<div class="row"><strong>Downloading <span class="mono">${esc(j.current || "…")}</span></strong>
          <span class="muted">${fmt.bytes(j.file_bytes)} of ${fmt.bytes(j.file_total)}${j.queue.length ? ` · ${j.queue.length} more queued` : ""}</span></div>
        <div class="progress"><div style="width:${pct}%"></div></div>`
      : `<div class="row"><strong>${j.failed.length ? "Finished with errors" : "Download complete"}</strong>
          <span class="muted">${j.done.length} saved to the library</span><span class="spacer"></span>
          ${j.done.length ? `<a class="button" href="#/library">Open library</a>` : ""}</div>`}
      ${failed}</div>`;
  }

  async function pollJob() {
    const j = await api("/api/device/download").catch(() => null);
    if (!alive || !j) return;
    renderJob(j);
    if (sessions) {
      let changed = false;
      for (const s of sessions) if (!s.in_library && j.done.includes(s.name)) { s.in_library = true; changed = true; }
      if (changed) renderList();
    }
    clearTimeout(polling);
    if (j.running) polling = setTimeout(pollJob, 500);
    ["#load", "#dl-new", "#dl-sel"].forEach((id) => { if (j.running) $(id).disabled = true; });
    if (!j.running && sessions) renderList();
  }

  async function start(names) {
    if (!names.length) return;
    try {
      await api("/api/device/download", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ names }) });
      pollJob();
    } catch (e) {
      $("#job").innerHTML = `<p class="error">${esc(e.message)}</p>`;
    }
  }

  $("#load").addEventListener("click", load);
  $("#dl-new").addEventListener("click", () => start(sessions.filter((s) => !s.in_library).map((s) => s.name)));
  $("#dl-sel").addEventListener("click", () => start([...document.querySelectorAll(".pick:checked")].map((p) => p.value)));

  device.listeners.add(renderStatus);
  renderStatus(device.status);
  pollJob();
  return () => { alive = false; clearTimeout(polling); device.listeners.delete(renderStatus); };
}

// ---------------------------------------------------------------------------------------
// session

const DEFAULT_CHANNELS = ["GPS Speed", "LateralAcc", "InlineAcc"];

function sessionView(view, name) {
  let alive = true, detail = null, lap = null, charts = [], track = null, markerT = null;
  const base = `/api/library/${encodeURIComponent(name)}`;
  view.innerHTML = `<div class="empty">Loading ${esc(name)}…</div>`;

  const onResize = () => { sizeCharts(); drawMap(); };
  window.addEventListener("resize", onResize);

  api(base).then((d) => {
    if (!alive) return;
    detail = d;
    render();
  }).catch((e) => { view.innerHTML = `<div class="empty error">${esc(e.message)}</div>`; });

  function render() {
    const d = detail;
    const title = d.venue || d.device?.track || d.name;
    const sub = [d.name, d.date && `${fmt.date(d.date)} ${d.time}`, d.duration_s && fmt.dur(d.duration_s * 1000), d.logger].filter(Boolean).map(esc).join(" · ");
    let html = `<div class="row" style="margin-bottom:16px">
      <div><h1>${esc(title)}</h1><p class="sub" style="margin:0">${sub}</p></div>
      <span class="spacer"></span>
      <a class="button" href="#/library">← Library</a>
      <a class="button" href="${base}/raw">Download .xrz</a></div>`;
    if (d.kind !== "session") {
      html += `<div class="card"><h2>${d.kind === "log" ? "This file is the logger's debug log, not a session" : "This file couldn't be read"}</h2>
        ${d.error ? `<p class="error">${esc(d.error)}</p>` : ""}
        ${d.text ? `<p class="muted">The Solo saved its text log under a session name, so it shows up in the list with 0 laps.</p><pre class="log">${esc(d.text)}</pre>` : ""}</div>`;
      view.innerHTML = html;
      return;
    }
    const full = d.laps.filter((l) => l.kind === "full");
    const best = d.laps.find((l) => l.best);
    if (d.repaired_bytes) {
      const lines = d.text ? d.text.split("\n").length : 0;
      html += `<div class="card"><details><summary><strong>Repaired:</strong>
        the logger wrote ${lines} lines of its debug log (${fmt.bytes(d.repaired_bytes)}) into this file.
        They're removed before reading; the downloaded file is unchanged.</summary>
        <pre class="log" style="margin-top:12px">${esc(d.text)}</pre></details></div>`;
    }
    html += `<div class="stats">
      <div class="stat"><div class="k">Timed laps</div><div class="v">${full.length}</div></div>
      <div class="stat"><div class="k">Best lap</div><div class="v">${best ? fmt.lap(best.duration_ms) : "–"}</div></div>
      <div class="stat"><div class="k">Duration</div><div class="v">${fmt.dur(d.duration_s * 1000)}</div></div>
      <div class="stat"><div class="k">Top speed</div><div class="v" id="top-speed">–</div></div>
      <div class="stat"><div class="k">Channels</div><div class="v">${d.channels.length}</div></div>
    </div>
    <div class="laps" id="laps">
      <button data-lap="" class="selected">Whole session</button>
      ${d.laps.map((l) => `<button data-lap="${l.num}" class="${l.best ? "best" : ""}">${l.kind === "out" ? "Out" : `Lap ${l.num}`} · ${fmt.lap(l.duration_ms)}</button>`).join("")}
    </div>
    <div class="session-grid">
      <div>
        ${d.has_gps ? `<div class="card map-card"><h2>Track</h2><canvas id="map"></canvas>
          <div class="legend"><span id="smin">–</span><div class="bar" style="background:linear-gradient(90deg,${SPEED_RAMP.join(",")})"></div><span id="smax">–</span></div></div>` : ""}
        ${lapTable(d)}
      </div>
      <div class="card">
        <div class="row" style="margin-bottom:4px"><h2 style="margin:0">Channels</h2><span class="spacer"></span><span class="muted" style="font-size:12px">Drag to zoom · double-click to reset</span></div>
        <div class="channels" id="channels">${d.channels.map((c, i) => `
          <label title="${esc(c.group)} · ${c.samples} samples · ${c.rate_hz.toFixed(1)} Hz">
            <input type="checkbox" value="${esc(c.id)}"><span class="swatch" style="background:${PALETTE[i % PALETTE.length]}"></span>${esc(c.name)}</label>`).join("")}
        </div>
        <div id="charts"></div>
      </div>
    </div>`;
    view.innerHTML = html;

    const wanted = store.get("lapanalysis.channels", DEFAULT_CHANNELS);
    view.querySelectorAll("#channels input").forEach((cb) => {
      const ch = d.channels.find((c) => c.id === cb.value);
      cb.checked = wanted.includes(ch.name);
      cb.addEventListener("change", () => {
        store.set("lapanalysis.channels", [...view.querySelectorAll("#channels input:checked")].map((x) => d.channels.find((c) => c.id === x.value).name));
        loadCharts();
      });
    });
    view.querySelectorAll("#laps button, tr[data-lap]").forEach((b) => b.addEventListener("click", () => selectLap(b.dataset.lap)));
    if (d.has_gps) api(`${base}/series?id=gps:speed&points=4000`).then((s) => {
      if (alive) $("#top-speed").textContent = fmt.num(Math.max(...s.v), "km/h");
    }).catch(() => {});
    selectLap("");
  }

  function lapTable(d) {
    if (!d.laps.length) return "";
    const best = d.laps.find((l) => l.best);
    return `<div class="card"><h2>Laps</h2><table>
      <thead><tr><th>Lap</th><th class="num">Time</th><th class="num">Δ best</th></tr></thead>
      <tbody>${d.laps.map((l) => `<tr class="link" data-lap="${l.num}">
        <td>${l.kind === "out" ? "Out" : l.num} ${l.best ? `<span class="chip good">best</span>` : ""}</td>
        <td class="num">${fmt.lap(l.duration_ms)}</td>
        <td class="num muted">${best && l.kind === "full" && !l.best ? `+${((l.duration_ms - best.duration_ms) / 1000).toFixed(3)}` : ""}</td></tr>`).join("")}
      </tbody></table></div>`;
  }

  function selectLap(value) {
    lap = value === "" ? null : Number(value);
    view.querySelectorAll("#laps button").forEach((b) => b.classList.toggle("selected", b.dataset.lap === (lap == null ? "" : String(lap))));
    loadTrack();
    loadCharts();
  }

  const lapQuery = () => (lap == null ? "" : `&lap=${lap}`);
  const lapSpan = () => {
    if (lap == null) return [0, detail.duration_s];
    const l = detail.laps.find((x) => x.num === lap);
    return [0, l.end_s - l.start_s];
  };

  // --- map ------------------------------------------------------------------------------

  async function loadTrack() {
    if (!detail.has_gps) return;
    try {
      track = await api(`${base}/track?points=4000${lapQuery()}`);
    } catch { track = null; }
    if (alive) drawMap();
  }

  function drawMap() {
    const canvas = $("#map");
    if (!canvas || !track) return;
    const dpr = window.devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
    canvas.width = w * dpr; canvas.height = h * dpr;
    const ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, w, h);
    const n = track.lat.length;
    if (n < 2) { ctx.fillStyle = cssVar("--muted"); ctx.fillText("No GPS fix in this lap", 12, 20); return; }
    const lat0 = track.lat.reduce((a, b) => a + b, 0) / n, k = Math.cos((lat0 * Math.PI) / 180);
    const xs = track.lon.map((v) => v * k), ys = track.lat;
    const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
    const pad = 16, scale = Math.min((w - 2 * pad) / (x1 - x0 || 1e-9), (h - 2 * pad) / (y1 - y0 || 1e-9));
    const ox = (w - (x1 - x0) * scale) / 2, oy = (h - (y1 - y0) * scale) / 2;
    const px = (i) => [ox + (xs[i] - x0) * scale, h - (oy + (ys[i] - y0) * scale)];
    const smin = Math.min(...track.speed_kmh), smax = Math.max(...track.speed_kmh);
    $("#smin").textContent = fmt.num(smin, "km/h");
    $("#smax").textContent = fmt.num(smax, "km/h");
    ctx.lineWidth = 3; ctx.lineCap = "round";
    for (let i = 1; i < n; i++) {
      const [ax, ay] = px(i - 1), [bx, by] = px(i);
      ctx.strokeStyle = speedColor((track.speed_kmh[i] - smin) / (smax - smin || 1));
      ctx.beginPath(); ctx.moveTo(ax, ay); ctx.lineTo(bx, by); ctx.stroke();
    }
    const [sx, sy] = px(0);
    ctx.fillStyle = cssVar("--text");
    ctx.beginPath(); ctx.arc(sx, sy, 4, 0, 2 * Math.PI); ctx.fill();
    if (markerT != null) {
      const i = nearest(track.t, markerT), [mx, my] = px(i);
      ctx.fillStyle = cssVar("--surface"); ctx.strokeStyle = cssVar("--text"); ctx.lineWidth = 2.5;
      ctx.beginPath(); ctx.arc(mx, my, 6, 0, 2 * Math.PI); ctx.fill(); ctx.stroke();
    }
  }

  // --- charts ---------------------------------------------------------------------------

  let loadSeq = 0;
  async function loadCharts() {
    const seq = ++loadSeq;
    const ids = [...view.querySelectorAll("#channels input:checked")].map((x) => x.value);
    const box = $("#charts");
    if (!ids.length) { destroyCharts(); box.innerHTML = `<div class="empty">Pick channels above to chart them.</div>`; return; }
    const width = Math.max(200, box.clientWidth);
    let series;
    try {
      series = await Promise.all(ids.map((id) => api(`${base}/series?id=${encodeURIComponent(id)}&points=${Math.round(width * 1.5)}${lapQuery()}`)));
    } catch (e) {
      box.innerHTML = `<div class="empty error">${esc(e.message)}</div>`;
      return;
    }
    if (!alive || seq !== loadSeq) return;
    destroyCharts();
    box.innerHTML = "";
    const [t0, t1] = lapSpan();
    let syncing = false;
    for (const s of series) {
      const idx = detail.channels.findIndex((c) => c.id === s.id);
      const color = PALETTE[idx % PALETTE.length];
      const wrap = document.createElement("div");
      wrap.className = "chart";
      wrap.innerHTML = `<div class="title"><span>${esc(s.name)}${s.units ? ` <span class="muted">${esc(s.units)}</span>` : ""}</span><span class="val">–</span></div>`;
      box.appendChild(wrap);
      const valEl = $(".val", wrap);
      const axis = { stroke: cssVar("--axis"), grid: { stroke: cssVar("--grid") }, ticks: { stroke: cssVar("--grid") } };
      const u = new uPlot({
        width, height: 150,
        cursor: { sync: { key: "solo" }, drag: { x: true, y: false } },
        legend: { show: false },
        // Every chart spans the same window so the synced cursor lines up across channels
        // sampled at different rates.
        scales: { x: { time: false, auto: false, range: (u, min, max) => [min ?? t0, max ?? t1] } },
        axes: [
          { ...axis, values: (u, vals) => {
            const step = vals.length > 1 ? vals[1] - vals[0] : 1;
            return vals.map((v) => fmt.clock(v, step < 1 ? 1 : 0));
          } },
          { ...axis, size: 50, values: (u, vals) => vals.map((v) => fmt.num(v)) },
        ],
        series: [{}, { stroke: color, width: 1.5, points: { show: false } }],
        hooks: {
          setCursor: [(u) => {
            const i = u.cursor.idx;
            valEl.textContent = i == null ? "–" : fmt.num(u.data[1][i], s.units);
            if (i != null && u === charts[0]) { markerT = u.data[0][i]; drawMap(); }
          }],
          setScale: [(u, key) => {
            if (key !== "x" || syncing) return;
            syncing = true;
            const { min, max } = u.scales.x;
            charts.forEach((o) => { if (o !== u) o.setScale("x", { min, max }); });
            syncing = false;
          }],
        },
      }, [s.t, s.v], wrap);
      u.over.addEventListener("dblclick", () => charts.forEach((o) => o.setScale("x", { min: t0, max: t1 })));
      charts.push(u);
    }
  }

  function sizeCharts() {
    const box = $("#charts");
    if (box) charts.forEach((u) => u.setSize({ width: Math.max(200, box.clientWidth), height: 150 }));
  }

  function destroyCharts() {
    charts.forEach((u) => u.destroy());
    charts = [];
  }

  return () => {
    alive = false;
    destroyCharts();
    window.removeEventListener("resize", onResize);
  };
}

// ---------------------------------------------------------------------------------------
// map colours

const SPEED_RAMP = ["#3b82f6", "#22c55e", "#eab308", "#f97316", "#ef4444"];

function speedColor(f) {
  f = Math.min(1, Math.max(0, f)) * (SPEED_RAMP.length - 1);
  const i = Math.min(SPEED_RAMP.length - 2, Math.floor(f)), r = f - i;
  const a = hex(SPEED_RAMP[i]), b = hex(SPEED_RAMP[i + 1]);
  return `rgb(${a.map((v, k) => Math.round(v + (b[k] - v) * r)).join(",")})`;
}

function hex(c) {
  return [1, 3, 5].map((i) => parseInt(c.slice(i, i + 2), 16));
}

function nearest(arr, t) {
  let lo = 0, hi = arr.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] < t) lo = mid + 1; else hi = mid;
  }
  return lo > 0 && Math.abs(arr[lo - 1] - t) < Math.abs(arr[lo] - t) ? lo - 1 : lo;
}

route();
