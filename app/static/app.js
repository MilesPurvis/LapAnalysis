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
    const t = Math.round(ms / 1000), h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), sec = t % 60;
    return h ? `${h}h ${m}m` : `${m}m ${String(sec).padStart(2, "0")}s`;
  },
  // When a session started, e.g. "Sep 7, 2026, 1:43 PM"; time of day, not a duration.
  started(iso, time) {
    if (!iso) return "–";
    const d = new Date(`${iso}T${time || "00:00:00"}`);
    if (isNaN(d)) return [iso, time].filter(Boolean).join(" ");
    const day = d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
    return time ? `${day}, ${d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit", hour12: true })}` : day;
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
  else if (page === "compare") cleanup = compareView(view, arg);
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
      <thead><tr><th>Session</th><th>Started</th><th>Track</th><th>Driver</th><th>Car</th><th class="num">Laps</th><th class="num">Best lap</th><th class="num">Duration</th></tr></thead>
      <tbody>${items.map((i) => `
        <tr class="link" data-name="${esc(i.name)}">
          <td>${i.title ? `<div class="title-cell">${esc(i.title)}</div><div class="mono muted small">${esc(i.name)}</div>` : `<span class="mono">${esc(i.name)}</span>`}
            ${i.kind !== "session" ? `<span class="chip warn">${i.kind === "log" ? "logger log" : "unreadable"}</span>` : ""}</td>
          <td>${fmt.started(i.date, i.time)}</td><td>${esc(i.track) || "–"}</td>
          <td>${esc(i.driver) || "–"}</td><td>${esc(i.vehicle) || "–"}</td>
          <td class="num">${i.kind === "session" ? i.laps : "–"}</td>
          <td class="num">${fmt.lap(i.best_ms)}</td>
          <td class="num">${fmt.dur(i.duration_ms)}</td>
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
      <thead><tr><th><input type="checkbox" id="all"></th><th>Session</th><th>Started</th><th>Track</th><th class="num">Laps</th><th class="num">Best lap</th><th class="num">Duration</th><th class="num">Size</th><th></th></tr></thead>
      <tbody>${sessions.map((s) => `
        <tr>
          <td><input type="checkbox" class="pick" value="${esc(s.name)}"></td>
          <td class="mono">${esc(s.name)}</td>
          <td>${fmt.started(s.date, s.time)}</td><td>${esc(s.track) || "–"}</td>
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

  function headHtml(d) {
    const title = d.title || d.venue || d.name;
    const sub = [d.title && d.venue, d.name, d.date && fmt.started(d.date, d.time), d.duration_s && fmt.dur(d.duration_s * 1000), fmt.bytes(d.size), d.logger]
      .filter(Boolean).map(esc).join(" · ");
    return `<div class="row" style="margin-bottom:16px">
      <div><h1>${esc(title)}</h1><p class="sub" style="margin:0">${sub}</p></div>
      <span class="spacer"></span>
      <a class="button" href="#/library">← Library</a>
      <a class="button" href="${base}/raw">Download .xrz</a></div>`;
  }

  function renderDetails(editing, flash = "") {
    const el = $("#details");
    if (!el) return;
    el.innerHTML = editing ? detailsForm(detail) : detailsView(detail, flash);
    if (!editing) {
      $("#edit-details").addEventListener("click", () => renderDetails(true));
      return;
    }
    const form = $("#details-form");
    $("[name=name]", form).focus();
    $("#cancel-details").addEventListener("click", () => renderDetails(false));
    form.addEventListener("keydown", (e) => { if (e.key === "Escape") renderDetails(false); });
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const f = new FormData(form);
      const num = (k) => (f.get(k) === "" ? null : Number(f.get(k)));
      const corners = (w) => ({ fl: num(`${w}.fl`), fr: num(`${w}.fr`), rl: num(`${w}.rl`), rr: num(`${w}.rr`) });
      const info = {
        name: f.get("name"), driver: f.get("driver"), vehicle: f.get("vehicle"), track: f.get("track"), notes: f.get("notes"),
        tyres: { compound: f.get("compound"), unit: f.get("unit"), cold: corners("cold"), hot: corners("hot") },
      };
      const status = $("#details-status");
      status.textContent = "Saving…";
      status.className = "muted";
      try {
        const r = await api(`${base}/info`, {
          method: "PUT", headers: { "content-type": "application/json" },
          body: JSON.stringify({ info, fill_day: f.get("fill_day") === "on" }),
        });
        if (!alive) return;
        detail = r.detail;
        $("#head").innerHTML = headHtml(detail);
        const others = r.filled ? ` · filled ${r.filled} other session${r.filled > 1 ? "s" : ""} from ${fmt.date(detail.date)}` : "";
        renderDetails(false, `Saved${others}`);
      } catch (err) {
        status.textContent = err.message;
        status.className = "error";
      }
    });
  }

  function render() {
    const d = detail;
    let html = `<div id="head">${headHtml(d)}</div>`;
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
    html += `<div id="details"></div>
    <div class="stats">
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
    renderDetails(false);

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
      <thead><tr><th>Lap</th><th class="num">Time</th><th class="num">Δ best</th><th></th></tr></thead>
      <tbody>${d.laps.map((l) => `<tr class="link" data-lap="${l.num}">
        <td>${l.kind === "out" ? "Out" : l.num} ${l.best ? `<span class="chip good">best</span>` : ""}</td>
        <td class="num">${fmt.lap(l.duration_ms)}</td>
        <td class="num muted">${best && l.kind === "full" && !l.best ? `+${((l.duration_ms - best.duration_ms) / 1000).toFixed(3)}` : ""}</td>
        <td class="num"><a class="small" href="#/compare/${encodeURIComponent(d.name)}:${l.num}" onclick="event.stopPropagation()">Compare</a></td></tr>`).join("")}
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
    const { ctx, w, h } = setupCanvas(canvas);
    const n = track.lat.length;
    if (n < 2) { ctx.fillStyle = cssVar("--muted"); ctx.fillText("No GPS fix in this lap", 12, 20); return; }
    const proj = makeProjection(w, h, [track.lat], [track.lon]);
    const px = (i) => proj(track.lat[i], track.lon[i]);
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
// compare

const LAP_COLORS = ["#e5484d", "#3b82f6", "#22c55e", "#f59e0b", "#a855f7"];
const MAX_COMPARE = LAP_COLORS.length;
const COMPARE_CHANNELS = ["GPS Speed", "LateralAcc", "InlineAcc"];

const lapName = (l) => (l.kind === "out" ? "Out lap" : l.kind === "in" ? "In lap" : `Lap ${l.num}`);

function parseLapSpec(arg) {
  return (arg || "").split(",").filter(Boolean).map((part) => {
    const i = part.lastIndexOf(":");
    return { file: decodeURIComponent(part.slice(0, i)), lap: Number(part.slice(i + 1)) };
  }).filter((p) => p.file && Number.isFinite(p.lap));
}

// Rough distance in km between two [lat, lon] points; plenty for "same track?".
function kmBetween(a, b) {
  if (!a || !b) return Infinity;
  const r = Math.PI / 180;
  return 6371 * Math.hypot((b[0] - a[0]) * r, (b[1] - a[1]) * r * Math.cos(a[0] * r));
}

function fmtGap(ms) {
  return `${ms >= 0 ? "+" : "−"}${(Math.abs(ms) / 1000).toFixed(3)}`;
}

function compareView(view, arg) {
  let alive = true, catalog = [], picked = parseLapSpec(arg), result = null, charts = [], cursor = null, sameTrack = true;
  view.innerHTML = `
    <h1>Compare laps</h1>
    <p class="sub">Laps are lined up by distance around the track. The first is the reference; the others are timed against it.</p>
    <div class="card" id="picked"><div class="empty">Loading laps…</div></div>
    <div id="result"></div>`;
  const onResize = () => { sizeCharts(); drawLines(); };
  window.addEventListener("resize", onResize);

  api("/api/laps").then((c) => {
    if (!alive) return;
    catalog = c;
    renderPicked();
    load();
  }).catch((e) => { $("#picked").innerHTML = `<div class="empty error">${esc(e.message)}</div>`; });

  const session = (file) => catalog.find((s) => s.name === file);
  const lapOf = (file, num) => session(file)?.laps.find((l) => l.num === num);

  function setPicked(next) {
    picked = next.slice(0, MAX_COMPARE);
    history.replaceState(null, "", `#/compare/${picked.map((p) => `${encodeURIComponent(p.file)}:${p.lap}`).join(",")}`);
    renderPicked();
    load();
  }

  function renderPicked() {
    const chips = picked.map((p, i) => {
      const s = session(p.file), l = lapOf(p.file, p.lap);
      const meta = [l && fmt.lap(l.duration_ms), s && fmt.started(s.date, s.time), s?.driver, s?.vehicle].filter(Boolean).map(esc).join(" · ");
      return `<div class="lap-chip">
        <span class="swatch" style="background:${LAP_COLORS[i]}"></span>
        <div><strong>${esc(s?.title || p.file)} · ${l ? lapName(l) : `Lap ${p.lap}`}</strong>${i === 0 ? ` <span class="chip">reference</span>` : ""}
          <div class="muted small">${meta}</div></div>
        <span class="spacer"></span>
        ${i ? `<button class="small-btn" data-ref="${i}">Set as reference</button>` : ""}
        <button class="small-btn" data-remove="${i}" aria-label="Remove lap">✕</button>
      </div>`;
    }).join("");
    $("#picked").innerHTML = `
      <div class="row"><h2 style="margin:0">Laps</h2><span class="muted small">${picked.length} of ${MAX_COMPARE}</span><span class="spacer"></span>
        <label class="check" style="margin:0"><input type="checkbox" id="same-track"${sameTrack ? " checked" : ""}> Same track only</label></div>
      ${chips ? `<div class="lap-chips">${chips}</div>` : `<p class="muted" style="margin:10px 0 0">Pick laps below, or use "Compare" in a session's lap list.</p>`}
      <div class="picker">${pickerHtml()}</div>`;
    view.querySelectorAll("[data-remove]").forEach((b) => b.addEventListener("click", () => setPicked(picked.filter((_, i) => i !== Number(b.dataset.remove)))));
    view.querySelectorAll("[data-ref]").forEach((b) => b.addEventListener("click", () => {
      const i = Number(b.dataset.ref);
      setPicked([picked[i], ...picked.filter((_, j) => j !== i)]);
    }));
    $("#same-track").addEventListener("change", (e) => { sameTrack = e.target.checked; renderPicked(); });
    view.querySelectorAll("[data-add]").forEach((b) => b.addEventListener("click", () => {
      const v = b.dataset.add, i = v.lastIndexOf(":"), p = { file: v.slice(0, i), lap: Number(v.slice(i + 1)) };
      const at = picked.findIndex((q) => q.file === p.file && q.lap === p.lap);
      setPicked(at >= 0 ? picked.filter((_, j) => j !== at) : [...picked, p]);
    }));
  }

  function pickerHtml() {
    const refPos = picked.length ? session(picked[0].file)?.position : null;
    const list = catalog.filter((s) => !sameTrack || !refPos || kmBetween(s.position, refPos) < 3);
    if (!list.length) return `<p class="muted" style="margin:0">No sessions with GPS in the library.</p>`;
    const full = picked.length >= MAX_COMPARE;
    return list.map((s) => `<div class="picker-row">
      <div><strong>${esc(s.title || s.name)}</strong>
        <div class="muted small">${[fmt.started(s.date, s.time), s.driver, s.vehicle].filter(Boolean).map(esc).join(" · ")}</div></div>
      <div class="laps">${s.laps.map((l) => {
        const on = picked.some((p) => p.file === s.name && p.lap === l.num);
        return `<button data-add="${esc(s.name)}:${l.num}" class="${on ? "selected" : l.best ? "best" : ""}"${full && !on ? " disabled" : ""}>${lapName(l)} · ${fmt.lap(l.duration_ms)}</button>`;
      }).join("")}</div></div>`).join("");
  }

  let seq = 0;
  async function load() {
    const mine = ++seq, box = $("#result");
    destroyCharts();
    result = null;
    if (picked.length < 2) {
      box.innerHTML = picked.length ? `<div class="card empty">Add another lap to compare against the reference.</div>` : "";
      return;
    }
    const available = [...new Set(picked.flatMap((p) => session(p.file)?.channels || []))];
    const chans = store.get("lapanalysis.compareChannels", COMPARE_CHANNELS).filter((c) => available.includes(c));
    box.innerHTML = `<div class="card empty">Lining up laps…</div>`;
    const laps = encodeURIComponent(picked.map((p) => `${p.file}:${p.lap}`).join(","));
    let r;
    try {
      r = await api(`/api/compare?laps=${laps}&channels=${encodeURIComponent(chans.join(","))}&points=1200`);
    } catch (e) {
      if (mine === seq) box.innerHTML = `<div class="card empty error">${esc(e.message)}</div>`;
      return;
    }
    if (!alive || mine !== seq) return;
    result = r;
    renderResult(available, chans);
  }

  function renderResult(available, chans) {
    const ref = result.laps[0];
    $("#result").innerHTML = `
      <div class="card"><h2>Summary</h2><div class="table-wrap"><table>
        <thead><tr><th></th><th>Lap</th><th>Driver</th><th>Car</th><th class="num">Time</th><th class="num">Gap</th><th class="num">Distance</th></tr></thead>
        <tbody>${result.laps.map((l, i) => {
          const lap = lapOf(l.name, l.lap) || { num: l.lap, kind: "full" };
          return `<tr><td><span class="swatch" style="background:${LAP_COLORS[i]}"></span></td>
            <td>${esc(l.title || l.name)} · ${lapName(lap)}</td><td>${esc(l.driver) || "–"}</td><td>${esc(l.vehicle) || "–"}</td>
            <td class="num">${fmt.lap(l.lap_ms)}</td><td class="num">${i ? fmtGap(l.lap_ms - ref.lap_ms) : "ref"}</td>
            <td class="num">${Math.round(l.length_m).toLocaleString()} m</td></tr>`;
        }).join("")}</tbody></table></div></div>
      <div class="session-grid">
        <div class="card map-card"><h2>Racing lines</h2><canvas id="cmp-map"></canvas>
          <p class="muted small" id="cursor-at">Hover a chart to follow the laps around the track.</p></div>
        <div class="card">
          <div class="row" style="margin-bottom:4px"><h2 style="margin:0">Channels</h2><span class="spacer"></span>
            <span class="muted small">Drag to zoom · double-click to reset</span></div>
          <div class="channels" id="cmp-channels">${available.map((name) => `
            <label><input type="checkbox" value="${esc(name)}"${chans.includes(name) ? " checked" : ""}><span class="swatch" style="background:var(--text)"></span>${esc(name)}</label>`).join("")}</div>
          <div id="cmp-charts"></div>
        </div>
      </div>`;
    view.querySelectorAll("#cmp-channels input").forEach((cb) => cb.addEventListener("change", () => {
      store.set("lapanalysis.compareChannels", [...view.querySelectorAll("#cmp-channels input:checked")].map((x) => x.value));
      load();
    }));
    const box = $("#cmp-charts");
    addChart(box, "Time gained / lost", "s", result.laps.map((l) => l.delta_s), (v) => fmtGap(v * 1000));
    for (const name of chans) addChart(box, name, result.units[name] || "", result.laps.map((l) => l.channels[name]), (v) => fmt.num(v));
    drawLines();
  }

  let syncing = false;
  function addChart(box, title, units, series, fmtValue) {
    const wrap = document.createElement("div");
    wrap.className = "chart";
    wrap.innerHTML = `<div class="title"><span>${esc(title)}${units ? ` <span class="muted">${esc(units)}</span>` : ""}</span><span class="vals"></span></div>`;
    box.appendChild(wrap);
    const vals = $(".vals", wrap), x = result.distance, end = x[x.length - 1];
    const axis = { stroke: cssVar("--axis"), grid: { stroke: cssVar("--grid") }, ticks: { stroke: cssVar("--grid") } };
    const u = new uPlot({
      width: Math.max(200, box.clientWidth), height: 150,
      cursor: { sync: { key: "compare" }, drag: { x: true, y: false } },
      legend: { show: false },
      scales: { x: { time: false, auto: false, range: (u, min, max) => [min ?? 0, max ?? end] } },
      axes: [
        { ...axis, values: (u, ticks) => ticks.map((v) => `${Math.round(v)} m`) },
        { ...axis, size: 50, values: (u, ticks) => ticks.map((v) => fmt.num(v)) },
      ],
      series: [{}, ...series.map((_, i) => ({ stroke: LAP_COLORS[i], width: i ? 1.5 : 2, dash: title.startsWith("Time") && !i ? [4, 4] : undefined, points: { show: false } }))],
      hooks: {
        setCursor: [(u) => {
          const i = u.cursor.idx;
          vals.innerHTML = i == null ? "" : series.map((s, k) => s[i] == null ? "" : `<span style="color:${LAP_COLORS[k]}">${esc(fmtValue(s[i]))}</span>`).join("");
          if (u === charts[0] && i !== cursor) { cursor = i; drawLines(); }
        }],
        setScale: [(u, key) => {
          if (key !== "x" || syncing) return;
          syncing = true;
          charts.forEach((o) => { if (o !== u) o.setScale("x", { min: u.scales.x.min, max: u.scales.x.max }); });
          syncing = false;
        }],
      },
    }, [x, ...series], wrap);
    u.over.addEventListener("dblclick", () => charts.forEach((o) => o.setScale("x", { min: 0, max: end })));
    charts.push(u);
  }

  function drawLines() {
    const canvas = $("#cmp-map");
    if (!canvas || !result) return;
    const { ctx, w, h } = setupCanvas(canvas);
    const has = (l, i) => l.lat[i] || l.lon[i];
    const proj = makeProjection(w, h, result.laps.map((l) => l.lat), result.laps.map((l) => l.lon));
    if (!proj) return;
    ctx.lineCap = "round";
    result.laps.forEach((l, li) => {
      ctx.strokeStyle = LAP_COLORS[li];
      ctx.lineWidth = li ? 2 : 3;
      ctx.beginPath();
      let drawing = false;
      l.lat.forEach((la, i) => {
        if (!has(l, i)) { drawing = false; return; }
        const [px, py] = proj(la, l.lon[i]);
        if (drawing) ctx.lineTo(px, py); else ctx.moveTo(px, py);
        drawing = true;
      });
      ctx.stroke();
    });
    const at = $("#cursor-at");
    if (cursor == null) return;
    result.laps.forEach((l, li) => {
      if (!has(l, cursor)) return;
      const [px, py] = proj(l.lat[cursor], l.lon[cursor]);
      ctx.fillStyle = LAP_COLORS[li]; ctx.strokeStyle = cssVar("--surface"); ctx.lineWidth = 2;
      ctx.beginPath(); ctx.arc(px, py, 5.5, 0, 2 * Math.PI); ctx.fill(); ctx.stroke();
    });
    if (at) at.textContent = `${Math.round(result.distance[cursor]).toLocaleString()} m into the lap`;
  }

  function sizeCharts() {
    const box = $("#cmp-charts");
    if (box) charts.forEach((u) => u.setSize({ width: Math.max(200, box.clientWidth), height: 150 }));
  }

  function destroyCharts() {
    charts.forEach((u) => u.destroy());
    charts = [];
    cursor = null;
  }

  return () => {
    alive = false;
    destroyCharts();
    window.removeEventListener("resize", onResize);
  };
}

// ---------------------------------------------------------------------------------------
// maps

function setupCanvas(canvas) {
  const dpr = window.devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
  canvas.width = w * dpr;
  canvas.height = h * dpr;
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  return { ctx, w, h };
}

// Flat projection of GPS points into a w×h box (longitude scaled by cos(latitude)),
// fitted around every point in the given sets. Points at 0,0 (no fix) are ignored.
function makeProjection(w, h, latSets, lonSets, pad = 16) {
  const lat = [], lon = [];
  latSets.forEach((la, s) => la.forEach((v, i) => {
    if (v || lonSets[s][i]) { lat.push(v); lon.push(lonSets[s][i]); }
  }));
  if (!lat.length) return null;
  const k = Math.cos(((lat.reduce((a, b) => a + b, 0) / lat.length) * Math.PI) / 180);
  const xs = lon.map((v) => v * k);
  const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...lat), Math.max(...lat)];
  const scale = Math.min((w - 2 * pad) / (x1 - x0 || 1e-9), (h - 2 * pad) / (y1 - y0 || 1e-9));
  const ox = (w - (x1 - x0) * scale) / 2, oy = (h - (y1 - y0) * scale) / 2;
  return (la, lo) => [ox + (lo * k - x0) * scale, h - (oy + (la - y0) * scale)];
}

// ---------------------------------------------------------------------------------------
// session details

const CORNERS = ["fl", "fr", "rl", "rr"];

function tyreSummary(t) {
  const set = (c) => (CORNERS.some((k) => c[k] != null) ? CORNERS.map((k) => c[k] ?? "–").join(" / ") : "");
  const cold = set(t.cold), hot = set(t.hot);
  const parts = [t.compound, cold && `cold ${cold}`, hot && `hot ${hot}`].filter(Boolean);
  return parts.join(" · ") + (cold || hot ? ` ${t.unit || "psi"}` : "");
}

function detailsView(d, flash) {
  const i = d.info;
  const rows = [["Driver", d.driver], ["Car", d.vehicle], ["Track", d.venue], ["Tyres", tyreSummary(i.tyres)]].filter(([, v]) => v);
  const empty = !rows.length && !i.notes && !i.name;
  return `<div class="card">
    <div class="row"><h2 style="margin:0">Details</h2>
      ${flash ? `<span class="muted small">${esc(flash)}</span>` : ""}
      <span class="spacer"></span><button id="edit-details">${empty ? "Add details" : "Edit"}</button></div>
    ${empty ? `<p class="muted" style="margin:8px 0 0">Give this session a name, and record who drove which car, tyres and notes.</p>`
      : `${rows.length ? `<dl class="details">${rows.map(([k, v]) => `<div><dt>${k}</dt><dd>${esc(v)}</dd></div>`).join("")}</dl>` : ""}
         ${i.notes ? `<div class="notes">${esc(i.notes)}</div>` : ""}`}
  </div>`;
}

function detailsForm(d) {
  const i = d.info, t = i.tyres, s = d.suggestions, n = d.same_day;
  const list = (id, vals) => `<datalist id="${id}">${vals.map((v) => `<option value="${esc(v)}">`).join("")}</datalist>`;
  const text = (name, value, placeholder, listId) =>
    `<input name="${name}" value="${esc(value)}" placeholder="${esc(placeholder)}" maxlength="120" autocomplete="off"${listId ? ` list="${listId}"` : ""}>`;
  const pressure = (when, k) =>
    `<td><input type="number" name="${when}.${k}" value="${t[when][k] ?? ""}" step="0.1" min="0" max="100" inputmode="decimal" aria-label="${when} ${k.toUpperCase()}"></td>`;
  return `<form class="card" id="details-form">
    <h2>Details</h2>
    <div class="form-grid">
      <label class="field wide">Session name ${text("name", i.name, "e.g. Practice 2")}</label>
      <label class="field">Driver ${text("driver", i.driver, d.driver || "Who drove", "dl-drivers")}</label>
      <label class="field">Car ${text("vehicle", i.vehicle, d.vehicle || "e.g. Miata NA", "dl-vehicles")}</label>
      <label class="field">Track ${text("track", i.track, d.logger_track || "Track name", "dl-tracks")}</label>
      <label class="field">Tyres ${text("compound", t.compound, "Compound, e.g. Hoosier A7", "dl-compounds")}</label>
      <div class="field wide">Pressures
        <table class="pressures">
          <thead><tr><th></th>${CORNERS.map((k) => `<th>${k.toUpperCase()}</th>`).join("")}<th></th></tr></thead>
          <tbody>
            <tr><th>Cold</th>${CORNERS.map((k) => pressure("cold", k)).join("")}
              <td rowspan="2"><select name="unit" aria-label="Pressure unit"><option value="psi">psi</option><option value="bar"${t.unit === "bar" ? " selected" : ""}>bar</option></select></td></tr>
            <tr><th>Hot</th>${CORNERS.map((k) => pressure("hot", k)).join("")}</tr>
          </tbody>
        </table>
      </div>
      <label class="field wide">Notes <textarea name="notes" rows="4" placeholder="Setup changes, what you worked on, how it felt">${esc(i.notes)}</textarea></label>
    </div>
    ${n ? `<label class="check"><input type="checkbox" name="fill_day" checked>
      Also fill driver, car and track on the ${n} other session${n > 1 ? "s" : ""} from ${fmt.date(d.date)} that don't have them yet</label>` : ""}
    <div class="row" style="margin-top:14px"><button class="primary" type="submit">Save</button>
      <button type="button" id="cancel-details">Cancel</button><span id="details-status" class="muted"></span></div>
    ${list("dl-drivers", s.drivers)}${list("dl-vehicles", s.vehicles)}${list("dl-tracks", s.tracks)}${list("dl-compounds", s.compounds)}
  </form>`;
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
