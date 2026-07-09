const { invoke } = window.__TAURI__.core;

/* ============================================================================
 * Aetheris — Instrument Deck renderer
 *
 * Renders the get_stats payload into a single-screen console. Two rules govern
 * everything here:
 *  1. HONESTY. Untrusted strings go through esc(). Missing data renders as
 *     "—"/"n/a"/"not linked" — never a fabricated 0. RUL low-confidence and
 *     coarse egress attribution are marked. Sources render in one of three
 *     states: live / unavailable (auto-detected absent) / needs-link (a real
 *     setup step exists).
 *  2. NO FLICKER. The DOM is built once (index.html); each 1 Hz poll updates
 *     scalar nodes in place and reconciles list rows by key, so panel scroll
 *     position is preserved and nothing flashes. (The old code rebuilt every
 *     panel's innerHTML every second.)
 * ==========================================================================*/

const $ = (sel) => document.querySelector(sel);

function esc(s) {
  return String(s ?? '')
    .replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}

// null/undefined -> em dash (honest "unknown"); real 0 -> "0 B".
function fmtBytes(n, dm = 1) {
  if (n == null || Number.isNaN(n)) return '—';
  if (n === 0) return '0 B';
  const k = 1024, u = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  const i = Math.min(u.length - 1, Math.floor(Math.log(Math.abs(n)) / Math.log(k)));
  return `${parseFloat((n / Math.pow(k, i)).toFixed(dm))} ${u[i]}`;
}

function setText(sel, text) { const el = $(sel); if (el) el.textContent = text; }

// ---- canvas instruments -----------------------------------------------------
const dpr = Math.max(1, window.devicePixelRatio || 1);
const col = (v) => getComputedStyle(document.documentElement).getPropertyValue(v).trim();

// Size the backing store to CSS pixels * dpr, reset transform, clear, return ctx.
function ctx2d(cv, cssW, cssH) {
  const w = cssW || cv.clientWidth || cv.width, h = cssH;
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr);
  }
  const c = cv.getContext('2d');
  c.setTransform(dpr, 0, 0, dpr, 0, 0);
  c.clearRect(0, 0, w, h);
  return { c, w, h };
}

// Arc gauge: value 0..1 swept 135deg..405deg. track always drawn (so an
// unavailable instrument still reads as a real, empty gauge). cssW/cssH are the
// display size; ctx2d scales the backing store by dpr for crisp HiDPI output,
// so we pin the CSS size here to keep the on-screen dimensions fixed.
function arcGauge(cv, val, color, cssW = 72, cssH = 60) {
  cv.style.width = cssW + 'px'; cv.style.height = cssH + 'px';
  const { c, w, h } = ctx2d(cv, cssW, cssH);
  const cx = w / 2, cy = h * 0.62, r = Math.min(w, h) * 0.42, a0 = Math.PI * 0.75, a1 = Math.PI * 2.25;
  c.lineWidth = 6; c.lineCap = 'round';
  c.strokeStyle = '#0a0d13'; c.beginPath(); c.arc(cx, cy, r, a0, a1); c.stroke();
  if (val != null && val > 0) {
    c.strokeStyle = color; c.beginPath(); c.arc(cx, cy, r, a0, a0 + (a1 - a0) * Math.max(0, Math.min(1, val))); c.stroke();
  }
  c.strokeStyle = 'rgba(255,255,255,.14)'; c.lineWidth = 1;
  for (let i = 0; i <= 8; i++) {
    const a = a0 + (a1 - a0) * i / 8;
    c.beginPath();
    c.moveTo(cx + Math.cos(a) * (r - 8), cy + Math.sin(a) * (r - 8));
    c.lineTo(cx + Math.cos(a) * (r - 3), cy + Math.sin(a) * (r - 3));
    c.stroke();
  }
}

function sparkline(cv, pts, color, cssH) {
  const { c, w, h } = ctx2d(cv, cv.clientWidth, cssH);
  if (!pts.length) return;
  const mx = Math.max(...pts, 1), n = pts.length;
  c.beginPath();
  pts.forEach((p, i) => {
    const x = n > 1 ? i / (n - 1) * w : 0, y = h - (p / mx) * (h - 3) - 1;
    i ? c.lineTo(x, y) : c.moveTo(x, y);
  });
  c.lineWidth = 1.5; c.strokeStyle = color; c.stroke();
  const g = c.createLinearGradient(0, 0, 0, h);
  g.addColorStop(0, color + '55'); g.addColorStop(1, color + '05');
  c.lineTo(w, h); c.lineTo(0, h); c.closePath(); c.fillStyle = g; c.fill();
  const last = pts[n - 1], ly = h - (last / mx) * (h - 3) - 1;
  c.fillStyle = color; c.beginPath(); c.arc(w - 1.5, ly, 2, 0, 7); c.fill();
}

// ---- keyed list reconciler --------------------------------------------------
// Reuse row nodes across polls: update in place, insert new, remove gone. Row
// contents are re-set (innerHTML of the row only), so the panel's scroll box is
// never cleared -> no flash, no scroll reset.
function reconcile(container, items, keyOf, tag, fillHTML) {
  const prev = new Map();
  for (const node of Array.from(container.children)) prev.set(node.dataset.k, node);
  const used = new Set();
  items.forEach((item, i) => {
    const k = String(keyOf(item, i));
    let node = prev.get(k);
    if (!node) { node = document.createElement(tag); node.dataset.k = k; }
    const html = fillHTML(item, i);
    if (node.innerHTML !== html) node.innerHTML = html;   // only touch DOM on change
    const className = item.__cls || '';
    if (node.className !== className) node.className = className;
    used.add(k);
    if (container.children[i] !== node) container.insertBefore(node, container.children[i] || null);
  });
  for (const [k, node] of prev) if (!used.has(k)) node.remove();
}

// client-side sparkline history (backend sends no history; build it honestly).
const cpuHist = [], netHist = [], HIST = 40;
function push(buf, v) { buf.push(v); if (buf.length > HIST) buf.shift(); }

// ---- main render ------------------------------------------------------------
function render(stats) {
  const s = stats.static || {}, d = stats.dynamic || {}, x = d.extras || {};
  let setup = 0, attention = 0;

  // ---- top bar (static-ish) ----
  setText('#m-host', s.os?.hostname ?? '—');
  setText('#m-os', `${s.os?.distro ?? '—'} · ${s.os?.arch ?? ''}`.trim());
  setText('#m-kernel', s.os?.kernel ?? '—');
  const up = d.uptime || 0;
  setText('#m-uptime', `${Math.floor(up / 3600)}h ${String(Math.floor((up % 3600) / 60)).padStart(2, '0')}m`);
  const t = new Date(stats.timestamp || Date.now());
  setText('#clock', [t.getHours(), t.getMinutes(), t.getSeconds()].map((n) => String(n).padStart(2, '0')).join(':'));

  // ---- CPU ----
  const load = d.cpu?.currentLoad;                     // absent -> "—", never a fabricated 0
  setText('#cpu-val', load != null ? load.toFixed(1) : '—');
  setText('#cpu-coren', `${s.cpu?.cores ?? (d.cpu?.currentLoadCpu?.length || '—')}c`);
  const cores = d.cpu?.currentLoadCpu || [];
  const host = $('#cpu-cores');
  if (host.childElementCount !== cores.length) {
    host.innerHTML = ''; cores.forEach(() => host.appendChild(document.createElement('i')));
  }
  cores.forEach((v, i) => { host.children[i].style.height = (18 + Math.max(0, Math.min(100, v)) * 0.62) + '%'; });
  if (load != null) push(cpuHist, load);
  sparkline($('#cpu-spark'), cpuHist, col('--accent'), 30);

  // ---- Thermal (hottest sensor) ----
  const sensors = x.sensors || [];
  const hot = sensors.filter((se) => se.temp != null).sort((a, b) => b.temp - a.temp)[0];
  if (hot) {
    const lim = hot.critical || hot.max || 100;
    const ratio = hot.temp / lim;
    const tcol = ratio >= 0.9 ? col('--crit') : ratio >= 0.7 ? col('--warn') : col('--good');
    setText('#therm-val', hot.temp.toFixed(0));
    setText('#therm-meta', `${hot.label || 'sensor'} · crit ${hot.critical ?? hot.max ?? '—'}`);
    arcGauge($('#therm-gauge'), ratio, tcol);
  } else {
    setText('#therm-val', '—'); setText('#therm-meta', 'no sensors');
    arcGauge($('#therm-gauge'), null, col('--faint'));
  }

  // ---- Memory ----
  const totalMem = s.mem?.total || 0, usedMem = d.mem?.used || 0;
  const memPct = d.mem?.percentUsed ?? (totalMem ? usedMem / totalMem * 100 : 0);
  setText('#mem-pct', `${memPct.toFixed(0)}%`);
  setText('#mem-val', `${fmtBytes(usedMem)} / ${fmtBytes(totalMem)}`);
  $('#mem-bar').style.width = `${Math.min(100, memPct)}%`;
  const swapT = d.mem?.swaptotal || 0, swapU = d.mem?.swapused || 0;
  const swapPct = d.mem?.swapPercentUsed ?? (swapT ? swapU / swapT * 100 : 0);
  setText('#swap-val', swapT ? `${fmtBytes(swapU)} / ${fmtBytes(swapT)}` : 'none');
  $('#swap-bar').style.width = `${Math.min(100, swapPct)}%`;

  // ---- Battery (vitals cell only when present; no empty panel) ----
  const batts = x.batteries || [];
  const battCell = $('#battery-cell');
  if (batts.length) {
    const b = batts[0], rul = b.rul || {};
    battCell.classList.remove('hidden');
    setText('#batt-soh', b.stateOfHealth != null ? b.stateOfHealth.toFixed(0) : '—');
    setText('#batt-charge', b.stateOfCharge != null ? `${b.stateOfCharge.toFixed(0)}%` : '—');
    const eol = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : '—';
    setText('#batt-meta', `${b.cycleCount ?? '—'} cyc · EOL ${eol}${rul.confidence === 'low' ? ' (est.)' : ''}`);
  } else {
    battCell.classList.add('hidden');
  }

  // ---- SSD endurance / RUL (worst-health disk) ----
  const disks = (x.smartDisks || []).filter((dk) => dk.rul);
  const disk = disks.sort((a, b) => (a.rul.healthPercent ?? 101) - (b.rul.healthPercent ?? 101))[0];
  if (disk) {
    const rul = disk.rul, hp = rul.healthPercent;
    const hcol = hp == null ? col('--faint') : hp >= 75 ? col('--good') : hp >= 50 ? col('--warn') : col('--crit');
    setText('#ssd-val', hp != null ? hp.toFixed(0) : '—');
    const eol = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : '—';
    setText('#ssd-eol', `EOL ${eol}${rul.confidence === 'low' ? ' · est' : ''}`);
    setText('#ssd-written', `${fmtBytes(disk.bytesWritten)} written`);
    arcGauge($('#ssd-dial'), hp != null ? hp / 100 : null, hcol, 72, 62);
  } else {
    setText('#ssd-val', '—'); setText('#ssd-eol', 'no SMART data'); setText('#ssd-written', '');
    arcGauge($('#ssd-dial'), null, col('--faint'), 72, 62);
  }

  // ---- Network ----
  let rx = 0, tx = 0;
  (d.network || []).forEach((n) => { rx += n.rx_sec || 0; tx += n.tx_sec || 0; });
  setText('#net-rx', `${fmtBytes(rx)}/s`);
  setText('#net-tx', `${fmtBytes(tx)}/s`);
  push(netHist, rx);
  sparkline($('#net-spark'), netHist, col('--good'), 26);

  // ---- Egress: accounting mode + provider bars + connection table ----
  const topo = x.egressTopology || [], byProc = x.egressByProcess || [], acct = x.egressAccounting;
  const isEbpf = acct === 'ebpf';
  const modeCls = 'badge ' + (isEbpf ? 'b-good' : 'b-mut');
  const modeTxt = isEbpf ? 'eBPF per-PID' : 'socket-level';
  for (const sel of ['#egress-mode-badge', '#egress-badge']) {
    const b = $(sel); b.className = modeCls; b.textContent = modeTxt;
  }
  // total egress bytes (skip nulls, never fabricate)
  let total = 0, counted = false;
  if (isEbpf && byProc.length) { byProc.forEach((p) => { if (p.bytesSent != null) { total += p.bytesSent; counted = true; } }); }
  else { topo.forEach((c) => { if (c.bytes_sent != null) { total += c.bytes_sent; counted = true; } }); }
  setText('#egress-total', counted ? fmtBytes(total) : '—');
  const shadow = topo.filter((c) => c.shadow_alert).length;
  const nProc = isEbpf ? byProc.length : topo.length;
  $('#egress-cell-meta').innerHTML = `${nProc} ${isEbpf ? 'processes' : 'connections'}` + (shadow ? ` · <span class="warn">${shadow} shadow</span>` : '');
  attention += shadow;

  // provider bars (aggregate topology by provider)
  const byProv = new Map();
  topo.forEach((c) => {
    if (c.bytes_sent == null) return;
    const key = c.provider || 'unknown';
    const e = byProv.get(key) || { bytes: 0, free: true };
    // Green = genuinely free (mesh). A cloud provider rounding to $0 is still
    // billable egress, so cost===0 does NOT count as free here.
    e.bytes += c.bytes_sent; e.free = e.free && c.is_mesh;
    byProv.set(key, e);
  });
  const provs = [...byProv.entries()].map(([name, v]) => ({ name, ...v })).sort((a, b) => b.bytes - a.bytes).slice(0, 6);
  const provMax = Math.max(1, ...provs.map((p) => p.bytes));
  reconcile($('#provbars'), provs, (p) => p.name, 'div', (p) => {
    p.__cls = 'provbar';
    return `<span class="sub2">${esc(p.name)}</span>`
      + `<span class="track"><i class="${p.free ? 'free' : ''}" style="width:${Math.max(3, p.bytes / provMax * 100)}%"></i></span>`
      + `<span class="r num sub2">${fmtBytes(p.bytes)}</span>`;
  });

  // connection table
  reconcile($('#egress-body'), topo, (c, i) => `${c.pid ?? 'x'}|${c.destination_ip ?? ''}|${c.process ?? ''}|${i}`, 'tr', (c) => {
    c.__cls = 'rowflag' + (c.shadow_alert ? ' crit' : '');
    const pid = c.pid != null ? `<span class="sub2"> ·${c.pid}</span>` : '<span class="sub2 crit"> ·no-pid</span>';
    const sent = c.bytes_sent != null ? fmtBytes(c.bytes_sent) : '<span class="dim" title="needs eBPF per-PID accounting">—</span>';
    let cost;
    if (c.estimated_cost_usd == null) cost = '<span class="sub2">n/a</span>';
    else if (c.is_mesh) cost = '<span class="good">free</span>';         // genuinely free mesh
    else if (c.estimated_cost_usd === 0) cost = '<span class="sub2">$0.00</span>'; // billable, rounds to zero
    else cost = `<span class="warn">$${c.estimated_cost_usd.toFixed(2)}</span>`;
    const fb = c.attribution_confidence === 'fallback' ? '<span class="sub2" title="coarse ranges; live provider lists still loading"> ~</span>' : '';
    return `<td>${c.shadow_alert ? '<span class="crit">⚠ </span>' : ''}${esc(c.process)}${pid}</td>`
      + `<td>${esc(c.destination_name)}${fb}<br><span class="sub2">${esc(c.destination_ip)}</span></td>`
      + `<td class="r num">${sent}</td><td>${esc(c.provider)}</td><td class="r num">${cost}</td>`;
  });
  if (!topo.length) $('#egress-body').innerHTML = '<tr><td colspan="5" class="empty-state">No active egress connections.</td></tr>';

  // ---- Processes ----
  const procs = d.processes?.listCpu || [];
  setText('#proc-meta', `${d.processes?.all ?? '—'} total · ${d.processes?.running ?? '—'} run`);
  reconcile($('#proc-body'), procs, (p) => p.pid, 'tr', (p) => {
    return `<td class="num sub2">${esc(p.pid)}</td><td>${esc(p.name)}</td>`
      + `<td class="r num${p.cpu > 50 ? ' crit' : ''}">${(p.cpu ?? 0).toFixed(1)}</td>`
      + `<td><span class="minibar"><i style="width:${Math.min(100, (p.cpu ?? 0) * 3)}%"></i></span></td>`
      + `<td class="r num sub2">${(p.mem ?? 0).toFixed(1)}%</td>`;
  });
  if (!procs.length) $('#proc-body').innerHTML = '<tr><td colspan="5" class="empty-state">No process data</td></tr>';

  // ---- Containers ----
  const cw = x.containers || {};
  const list = cw.status === 'ok' ? (cw.containers || []) : null;
  if (list && list.length) {
    const running = list.filter((c) => c.state === 'running').length;
    const bad = list.filter((c) => c.state !== 'running' || (c.restartCount || 0) > 3 || c.health === 'unhealthy').length;
    attention += bad;
    $('#cont-meta').innerHTML = `<span class="good">${running} up</span>` + (list.length - running ? ` · <span class="crit">${list.length - running} down</span>` : '');
    reconcile($('#cont-body'), list, (c) => c.name, 'tr', (c) => {
      const sick = c.state !== 'running', warn = (c.restartCount || 0) > 3 || c.health === 'starting';
      c.__cls = 'rowflag' + (sick || c.health === 'unhealthy' ? ' crit' : warn ? ' warn' : '');
      const dot = c.health === 'healthy' ? col('--good') : sick || c.health === 'unhealthy' ? col('--crit') : c.health === 'starting' ? col('--warn') : col('--muted');
      const cpu = c.cpuPercent != null ? `${c.cpuPercent.toFixed(1)}%` : '—';
      const mem = c.memUsed != null ? fmtBytes(c.memUsed) : '—';
      const upd = c.imageUpdateAvailable === true ? '<span class="up">⬆</span>' : c.imageUpdateAvailable === false ? '' : '<span class="sub2">?</span>';
      return `<td>${esc(c.name)}<br><span class="sub2">${esc(c.image)}</span></td>`
        + `<td><span class="chip"><span class="sd" style="background:${dot}"></span><span class="${sick ? 'crit' : ''}">${esc(c.state)}</span></span></td>`
        + `<td class="r num">${cpu}</td><td class="r num">${mem}</td>`
        + `<td class="r num${warn ? ' warn' : ''}">${c.restartCount != null ? c.restartCount : '—'}</td><td>${upd}</td>`;
    });
  } else {
    $('#cont-meta').textContent = cw.status === 'ok' ? '0 running' : 'unavailable';
    const reason = cw.reason ? ` (${esc(cw.reason)})` : '';
    $('#cont-body').innerHTML = `<tr><td colspan="6" class="empty-state">${cw.status === 'ok' ? 'No running containers' : 'Docker not available' + reason}</td></tr>`;
  }

  // ---- GPU ----
  const gpus = x.gpus || [];
  if (gpus.length) {
    reconcile($('#gpu-rows'), gpus, (g, i) => `${g.vendor}|${g.model}|${i}`, 'div', (g) => {
      g.__cls = 'kv';
      const load = g.load != null ? `${g.load.toFixed(0)}%` : '—';
      const temp = g.temp != null ? `${g.temp.toFixed(0)}°C` : '—';
      return `<span class="k">${esc(g.vendor)} ${esc(g.model)}</span><span class="v num">${load} · ${temp}</span>`;
    });
  } else {
    $('#gpu-rows').innerHTML = '<div class="kv na"><span class="k">GPU</span><span class="v">no device on this host</span></div>';
  }

  // ---- AI proxy (live vs needs-link) ----
  const ai = x.aiProxy || {};
  const linked = (ai.samples || 0) > 0 && ai.lastTokensPerSec != null;
  const aiRow = $('#aiproxy-row'), aiV = aiRow.querySelector('.v'), aiHint = $('#aiproxy-hint'), aiBadge = $('#aiproxy-badge');
  if (linked) {
    aiRow.className = 'kv';
    aiV.textContent = `${ai.lastTokensPerSec.toFixed(1)} tok/s · ${ai.samples} obs`;
    aiHint.style.display = 'none';
    aiBadge.className = 'badge b-good'; aiBadge.textContent = 'live';
  } else {
    aiRow.className = 'kv unlinked';
    aiV.textContent = '— not linked';
    aiHint.style.display = 'flex';
    $('#aiproxy-hint-text').innerHTML = `Proxy is up on <span class="env">${esc(ai.proxyAddr || '127.0.0.1:3030')}</span> but sees no traffic. Route Ollama / LM Studio calls through it to observe tokens/sec.`;
    aiBadge.className = 'badge b-link'; aiBadge.textContent = '◆ proxy';
    setup += 1;
  }

  // ---- Arena ranks ----
  const bl = x.externalBaselines || {};
  const ev = bl.ai_evals || {};
  const arena = ev.lmsys_chat_arena || [];
  if (ev.status === 'ok' && arena.length) {
    setText('#arena-asof', ev.last_updated || 'live');
    reconcile($('#arena-body'), arena.slice(0, 3), (a) => `${a.rank}|${a.model}`, 'div', (a) => {
      a.__cls = 'kv';
      return `<span class="k">#${esc(a.rank)} ${esc(a.model)}</span><span class="v num${a.rank === 1 ? ' accent' : ''}">${a.score != null ? esc(a.score) : '—'}</span>`;
    });
  } else {
    setText('#arena-asof', 'unavailable');
    $('#arena-body').innerHTML = `<div class="kv na"><span class="k">Live ranks</span><span class="v">unavailable${ev.reason ? ' (' + esc(ev.reason) + ')' : ''}</span></div>`;
  }

  // ---- DORA (mixed live / needs-link) ----
  const dora = bl.dora || {}, lm = dora.local_metrics || {};
  const isNa = (v) => String(v ?? 'n/a').startsWith('n/a');
  const hasRepo = lm.status !== 'unavailable';   // availability lives on local_metrics.status
  const setDora = (sel, val) => {
    const row = $(sel), v = row.querySelector('.v');
    if (isNa(val)) { row.className = 'kv unlinked'; v.textContent = '◆ not linked'; v.title = String(val ?? ''); }
    else { row.className = 'kv'; v.textContent = String(val); v.title = ''; }
  };
  setDora('#dora-deploy', lm.deploy_frequency);
  setDora('#dora-lead', lm.lead_time);
  setDora('#dora-cfr', lm.change_failure_rate);
  setDora('#dora-mttr', lm.mttr);
  const needsToken = isNa(lm.change_failure_rate) || isNa(lm.mttr);
  const dBadge = $('#dora-badge'), dHint = $('#dora-hint'), dHintText = $('#dora-hint-text');
  if (!hasRepo) {
    dBadge.className = 'badge b-link'; dBadge.textContent = '◆ needs setup';
    dHint.style.display = 'flex'; dHintText.innerHTML = 'No git repository in the working directory. Run Aetheris inside your repo to compute DORA.';
    setup += 1;
  } else if (needsToken) {
    dBadge.className = 'badge b-link'; dBadge.textContent = '◆ partial';
    dHint.style.display = 'flex'; dHintText.innerHTML = 'CI failure metrics need the GitHub Actions API. Set <span class="env">GITHUB_TOKEN</span> or run <span class="env">gh auth login</span>.';
    setup += 1;
  } else {
    dBadge.className = 'badge b-good'; dBadge.textContent = lm.tier_rating || 'ok';
    dHint.style.display = 'none';
  }

  // ---- Pricing (reference) ----
  const pricing = bl.observability_pricing || [];
  reconcile($('#pricing-body'), pricing, (p) => p.tool, 'tr', (p) => {
    const free = p.est_monthly_cost === '$0';
    return `<td class="${free ? 'good' : ''}" style="${free ? 'font-weight:700' : ''}">${esc(p.tool)}</td>`
      + `<td class="sub2">${esc(p.tier)}</td><td class="r num${free ? ' good' : ''}">${esc(p.est_monthly_cost)}</td>`;
  });

  // ---- roll-ups ----
  const setupChip = $('#setup-chip');
  setupChip.classList.toggle('hidden', setup === 0);
  setText('#setup-count', setup);
  const lamp = $('#alert-lamp');
  lamp.classList.toggle('hot', attention > 0);
  lamp.classList.toggle('ok', attention === 0);
  setText('#alert-count', attention);
}

// ---- poll loop --------------------------------------------------------------
function tick() {
  invoke('get_stats').then((stats) => {
    try { render(stats); }
    catch (e) { console.error('render error', e); }
  }).catch((e) => console.error('get_stats failed', e));
}

tick();
setInterval(tick, 1000);
