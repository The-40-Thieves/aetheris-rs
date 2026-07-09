# Instrument-Deck UI Redesign — Design

**Goal:** Replace the glassmorphic auto-fit dashboard with a single-screen
"instrument deck" console that reflects importance/density in layout, surfaces
an anomaly hierarchy, adds light data-viz, encodes honest source states, and
eliminates the 1 Hz full-`innerHTML` re-render (flicker + scroll loss).

**Scope:** Frontend only — `src/index.html`, `src/styles.css`, `src/main.js`.
No change to the Rust backend or the `get_stats` payload. This is a rendering
change over the *existing, already-honest* data contract.

## Locked visual language (approved via Artifact mockup)

- **Instrument deck** aesthetic: monitoring console, system **monospace**
  everywhere with `tabular-nums`, uppercase letter-spaced labels, canvas
  gauges/sparklines, faint panel grid. No glassmorphism, no webfont (CSP blocks
  font CDNs; system mono stack avoids a silent fallback).
- **Single-screen console**: fixed `100vh`, `body { overflow: hidden }`. A
  pinned **vitals strip** over a 3-panel deck (processes · egress+cost ·
  containers) over a bottom strip (GPU/AI · DORA · pricing). Long lists scroll
  **within** their panel, never the page.
- **Instrument amber** accent `#f5a623` on cool near-black ground `#0c0e12`,
  held to chrome + primary readouts. Semantic colors are **separate** from the
  accent: good `#3ecf7a`, warn `#ff7d3b`, crit `#ff4d4f`. Committed dark,
  single-theme (an instrument deck is inherently a dark console — a deliberate
  choice, not an omission).

## Layout

```
┌ topbar: wordmark · host/arch/uptime/kernel · legend · ◆SETUP n · ATTENTION n · LIVE clock ┐
├ vitals: CPU · Thermal · Memory · [Battery*] · SSD-RUL · Network · Egress-mode  (*only if present) ┤
├ deck:   Processes        | Egress topology + cost | Containers   (each scroll-within)            ┤
├ strip:  GPU · AI proxy    | DORA · this repo       | Observability cost (reference)              ┤
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

The empty Battery panel is **removed**: battery is a vitals cell rendered only
when `extras.batteries` is non-empty (desktop → no cell, not an empty box).

## Source-state model (the honesty requirement)

Every data source renders as exactly one of three states — conflating them is
the lie the app was de-mocked to avoid. Encoded in `main.js`, signalled by a
top-bar legend:

| State | When | Treatment |
|---|---|---|
| **● live** | source detected + flowing | normal readout |
| **○ unavailable** | auto-detected absent, *no action* | dim/italic or omitted (GPU with no device, no sensors, no SMART, no battery) |
| **◆ needs link** | a real setup step exists | amber affordance naming the exact fix |

Two **distinct** top-bar indicators (a to-do is not a fault):
- `◆ SETUP n` — count of *linkable* sources (dashed amber, quiet).
- `ATTENTION n` — count of *faults* (solid; exited/unhealthy containers,
  restartCount > 3, shadow-alert egress).

### Needs-link detection (bound to real payload fields)

- **AI proxy** → `extras.aiProxy.samples === 0` (or `lastTokensPerSec == null`).
  Affordance: "Route Ollama / LM Studio calls through `<proxyAddr>`."
- **DORA CFR / MTTR** → the metric string starts with `"n/a"`
  (`change_failure_rate` / `mttr` are `"n/a (requires CI integration)"` etc.
  until a GitHub token exists). Affordance: "Set `GITHUB_TOKEN` or run
  `gh auth login`." A repo-absent payload (`dora.status === 'unavailable'`,
  all four `"n/a"`) → "Run inside a git repository."

## Honesty invariants (carried over verbatim from current main.js)

- **`esc()`** every untrusted string before it touches `innerHTML`: process /
  container names + images, device models, sensor labels, destination
  name/IP, provider, model names, tool/tier, container reason.
- Null is never a fabricated `0`: egress `bytes_sent == null` → `—`;
  `estimated_cost_usd == null` → `n/a`; `is_mesh || cost === 0` → `Free`
  (green); else `$x.xx` (amber).
- RUL `confidence === 'low'` → append `(est.)`. Egress
  `attribution_confidence === 'fallback'` → append `~`.
- Egress accounting badge from `extras.egressAccounting`: `ebpf` (accent,
  "per-PID") vs `sockets` (muted, "socket-level").
- Provider bars: aggregate `egressTopology` by `provider` summing
  `bytes_sent` (skip null); accent amber, except mesh/free tinted green
  (free = a genuine good-semantic).

## Diff-render architecture (kills the flicker)

Current code rebuilds each panel's full `innerHTML` every second → flash +
resets scroll inside panels. Replace with:

- **Scalar binds**: fixed nodes (CPU %, mem, gauges, counts, clock) updated by
  `textContent` / canvas redraw. Canvas gauges fully clear+redraw each poll
  (a tiny gauge — no flicker).
- **Keyed row reconciler** for every list (processes by `pid`, egress by
  `pid|ip`, containers by `name`, provider bars by `provider`, arena by
  `rank|model`, pricing by `tool`): reuse existing row nodes, update in place,
  insert new, remove gone. Because row nodes persist and only their contents
  update, the panel `.body` scroll position is preserved and there is no
  whole-body flash.
- **Client ring buffers** (cap ~40 samples) for the CPU-load and network-rx
  sparklines. The backend sends no history, so the trend builds up honestly
  *after* launch — no fabricated back-fill; a short/absent line on first paint.

## Data-viz instruments (canvas, per design-skill preference over hand SVG)

- CPU: big % + per-core bars (`currentLoadCpu`) + load sparkline (ring buffer).
- Thermal: arc gauge, `hottest sensor.temp` over its `critical` (or `max`);
  zone-colored. Unavailable when no sensors.
- SSD endurance: arc dial from `smartDisks[].rul.healthPercent`; EOL + TBW;
  `(est.)` on low confidence. Unavailable when no SMART disks.
- Network: ↓/↑ rates + rx sparkline. Memory: used/total + swap sub-bar.
- Egress: horizontal provider bars.

## Verification

No unit-testable Rust change. Verify by rendering the real
`index.html`/`styles.css`/`main.js` headlessly against a mock payload that
matches the exact `get_stats` contract (stubbed `window.__TAURI__`), at
1440×900, and confirm: single-screen (no page scroll), all three source states
visible, anomaly + setup counts correct, no console errors, `esc()` on all
untrusted fields. Then an independent code review before PR.

## Out of scope

Light theme; backend/payload changes; container control; any new data source.
