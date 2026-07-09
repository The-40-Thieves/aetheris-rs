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

### External baselines
- **LLM leaderboard ranks** fetched live from the Arena community mirror and
  cached; on fetch failure the panel shows *"unavailable"*, never stale/fake ranks.
- **DORA metrics** computed from real `git log` (deploy frequency + lead-time
  proxy). Change-failure-rate and MTTR are shown as *n/a* because they require CI
  / incident data not wired in.
- **Chip Tjmax limits and SaaS pricing** are genuinely static reference data and
  are labelled as such (`as_of` + `source`).

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

¹ Apple GPU via `powermetrics` (requires root).

## Roadmap (planned, not yet implemented)

- **eBPF egress byte accounting (Linux).** The current `/proc`+`ss` reader sees
  live sockets only. True per-PID byte accounting across short-lived/closed
  sockets needs an in-kernel probe. The intended design uses [aya](https://aya-rs.dev)
  (pure-Rust eBPF, `CgroupSkb` egress hook) — requires root/`CAP_BPF`
  (+`CAP_NET_ADMIN`), kernel 5.8+, and a `bpf-linker` toolchain. Prior art:
  [domcyrus/rustnet](https://github.com/domcyrus/rustnet) (Apache-2.0), cited for
  its cross-platform per-process-attribution design — no code copied.
- **macOS / Windows egress** via PKTAP and `GetExtendedTcpTable` respectively.
- **CI-integrated DORA** (real change-failure-rate / MTTR from a CI provider API).
- **Live SaaS pricing** (currently curated static reference).
- **Container-level monitoring** (the `containers` field is a stub, not a feature).

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
