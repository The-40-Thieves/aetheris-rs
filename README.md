# Aetheris Telemetry

Aetheris is a cross-platform machine-telemetry client built with Rust and Tauri.
It combines standard hardware monitoring with predictive endurance analytics,
per-process egress attribution, and local LLM inference observability.

> **Honesty note.** Aetheris was refactored to remove features that were
> previously hardcoded mocks. This README describes what is **implemented and
> verified** vs. what is **planned**. Where a feature depends on hardware or an
> OS not available for testing, that is called out explicitly.

## Implemented

### Hardware telemetry
- **CPU / memory / swap / network / processes / sensors** via `sysinfo`, refreshed
  live each poll.
- **GPU** — NVIDIA (`nvidia-smi` CSV), AMD (`rocm-smi --json`, with an `amd-smi`
  fallback), Apple Silicon (`powermetrics` plist, active residency = 1 − idle).
  VRAM is normalised to bytes; unknown metrics report `null`, not a fake `0`.
  Parsers are unit-tested against captured sample output; live values require the
  respective GPU/OS.
- **SMART endurance** — reads `smartctl -j`, branching by device class: NVMe
  (`data_units_written`) **and** SATA (`Total_LBAs_Written`, id 241/246, plus the
  Intel `Host_Writes_32MiB` and Kingston `Lifetime_Writes_GiB` unit variants).
  Previously SATA drives silently reported 0 bytes written.
- **Battery** — state-of-health, charge, and cycle count via `starship-battery`.

### Predictive RUL (Remaining Useful Life)
- Samples (SSD bytes written, battery SOH) are written to a local SQLite
  `telemetry` table on a throttled cadence, and End-of-Life is projected from the
  **measured** per-day velocity once ≈7 days of history exist.
- With insufficient history it falls back to a documented default and marks the
  result `confidence: "low"` (shown as *"(est.)"* in the UI), so a placeholder is
  never presented as a real trend.

### AI proxy observability
- An embedded Axum reverse-proxy (loopback `127.0.0.1:3030`) that **actually
  forwards** requests to a local engine and streams the response straight back:
  - `/ollama/*` → Ollama (default `127.0.0.1:11434`)
  - `/lmstudio/*` → LM Studio (default `127.0.0.1:1234`)
  - Override with `AETHERIS_OLLAMA_URL` / `AETHERIS_LMSTUDIO_URL`.
- It observes token accounting on the way through — Ollama `eval_count` /
  `eval_duration` (tokens/sec), LM Studio OpenAI `usage` — and logs it to
  telemetry. If the upstream engine is down it returns `502`, not a fake `200`.

### Egress topology (Linux)
- Real per-connection attribution from `/proc/net/tcp{,6}` (connections + socket
  inode), `/proc/<pid>/fd` (owning process, best-effort), and `ss -tinH`
  (cumulative `bytes_sent`/`bytes_acked` from kernel `tcp_info`).
- Destination IPs are classified (AWS / Azure / GCP / Cloudflare / Tailscale-mesh
  / private / unknown) via longest-prefix match against the live AWS `ip-ranges`
  and GCP `cloud.json` lists (with a coarse hardcoded fallback until they load).
- Egress cost = real bytes × provider $/GB; mesh traffic is free; unattributed
  destinations report `null` cost rather than a fabricated figure. When nothing
  qualifies it returns an empty list.
- **True per-PID byte accounting (eBPF, opt-in).** Built with `--features ebpf`
  and run with root / `CAP_BPF`+`CAP_NET_ADMIN`, an aya (pure-Rust eBPF) probe
  attaches kprobes to `tcp_sendmsg`/`udp_sendmsg` and accumulates a per-process
  `pid -> bytes` map — giving cumulative egress totals *including* short-lived /
  already-closed sockets and daemons with no currently-tracked connection, which
  the `ss` view cannot see. Surfaced as `extras.egressByProcess` with the mode in
  `extras.egressAccounting` (`"ebpf"` vs `"sockets"`). It counts the requested
  `sendmsg` application-layer payload size — not wire bytes (no IP/TCP headers or
  retransmits) — so it slightly over-counts partial/failed sends and, under heavy
  concurrent multithreaded sends, may modestly under-count (a non-atomic map
  update; a per-CPU map is future work). A `sched_process_exit` tracepoint evicts
  each process's entry on exit, so recycled PIDs never inherit a dead process's
  total. Without the feature or without privilege it is a silent no-op and the
  socket-level view is used — never fabricated. Building the probe needs a nightly
  toolchain with `rust-src` and `bpf-linker`; the arch is derived automatically so
  it works on x86_64 and aarch64. See `src-tauri/ebpf/`. (First `--features ebpf`
  build on a cold cache: if the nested probe build stalls fetching deps, run
  `cd src-tauri/ebpf && cargo fetch` once beforehand.)

### External baselines
- **LLM leaderboard ranks** fetched live from the Arena community mirror and
  cached; on fetch failure the panel shows *"unavailable"*, never stale/fake ranks.
- **DORA metrics** computed from real `git log` (deploy frequency + lead-time
  proxy) plus **real change-failure-rate and MTTR from the GitHub Actions API**
  when the working directory is a GitHub repo and a token is available
  (`GITHUB_TOKEN` or the `gh` CLI): CFR = failed / completed default-branch runs,
  MTTR = median time from a failing run to the next passing run. Falls back to
  *n/a* (never fabricated) when there's no repo, token, or CI runs.
- **Chip Tjmax limits and SaaS pricing** are genuinely static reference data and
  are labelled as such (`as_of` + `source`).

### Containers (Linux/any Docker host)
- Real per-container telemetry via the Docker CLI (auto-falling back to the
  Engine API over the socket via bollard): name, image, state, health, CPU%,
  memory, network/block I/O, PIDs, restart count, ports, uptime.
- **Image-update detection**: compares each image's local digest to the
  registry's current manifest digest (anonymous bearer-token auth), cached per
  image ~45 min. `true`/`false` only on a real compare; `null` when unknown.

### Security
- Proxy binds **loopback only** (was `0.0.0.0`, an open unauthenticated proxy).
- A restrictive **Content-Security-Policy** is set (was `null`); capabilities are
  reduced to `core:default`.
- Untrusted strings (process names, device models, scraped content) are
  HTML-escaped before rendering.

## Platform support

| Feature                 | Linux | macOS | Windows |
|-------------------------|:-----:|:-----:|:-------:|
| CPU/mem/net/sensors     |  ✅   |  ✅   |   ✅    |
| GPU (NVIDIA/AMD/Apple)  |  ✅   |  ✅¹  |   ✅    |
| SMART / battery         |  ✅   |  ✅   |   ✅    |
| AI proxy                |  ✅   |  ✅   |   ✅    |
| Egress topology         |  ✅   |  🔜   |   🔜    |
| Containers              |  ✅   |  ✅   |   ✅    |

¹ Apple GPU via `powermetrics` (requires root).

## Roadmap (planned, not yet implemented)

- **macOS / Windows egress** via PKTAP and `GetExtendedTcpTable` respectively.
- **Live SaaS pricing** (currently curated static reference).

## Requirements

To extract the most accurate data (e.g. `smartctl` metrics, `powermetrics` on
macOS), **run Aetheris with Administrator / root privileges**. Note that some
sources need elevation (`smartctl`, `powermetrics`, and privileged egress
attribution of other users' sockets) while `sysinfo`-based stats do not — so the
data shown depends on how it is launched.

- [Node.js](https://nodejs.org) and [Rust & Cargo](https://rustup.rs/)
- `smartctl` (SSD endurance), `nvidia-smi` / `rocm-smi` (GPU), `ss` (egress bytes)

## Getting Started

1. **Install dependencies**
   ```bash
   git clone https://github.com/The-40-Thieves/aetheris-rs.git
   cd aetheris-rs
   npm install
   ```
2. **Run the dev build** (use `sudo -E` on Linux/Mac for SMART/`powermetrics`/
   full egress attribution)
   ```bash
   npm run dev
   ```
3. **Build the app**
   ```bash
   npm run build
   ```
