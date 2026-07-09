# Aetheris — Repository Analysis (against the project plan)

**Analyzed:** 2026-07-09 · **Branch:** `claude/repo-analysis-project-setup-dhfv7o` (even with `master` @ `826b33f`)
**Method:** static read of the tree + source (no compile in this environment — see *Caveats*).

This document verifies the state of `aetheris-rs` against the shipped
project plan, maps the architecture, confirms the plan's claims against the
actual code, and records the (small) gaps found.

---

## 1. Verdict

The repository matches the plan. All 7 PRs described in the plan are merged
into `master`, the tree is clean, and the load-bearing claims — security
hardening, the honesty invariant, the eBPF opt-in, cross-platform gating —
are backed by real code, not prose. The one substantive finding is a single
remaining literal-`0` placeholder in the backend (`currentLoadUser` /
`currentLoadSystem`), noted in §5.

---

## 2. Architecture map

```
aetheris-rs/
├── src/                         # Frontend (instrument-deck console)
│   ├── index.html               #   DOM built once (169 lines)
│   ├── main.js                  #   diff-render + honesty formatting (404 lines)
│   └── styles.css               #   amber-on-near-black, single dark theme (139 lines)
├── src-tauri/                   # Rust / Tauri 2 backend
│   ├── src/
│   │   ├── main.rs              #   single `get_stats` command + cache + bg tasks (490)
│   │   ├── database.rs          #   local SQLite (rusqlite bundled) (162)
│   │   ├── server/mod.rs        #   axum loopback proxy 127.0.0.1:3030 (32)
│   │   ├── analytics/rul.rs     #   history-based RUL projection (217)
│   │   └── monitors/
│   │       ├── gpu.rs           #   nvidia-smi / rocm-smi / powermetrics (357)
│   │       ├── smart_disk.rs    #   smartctl -j, NVMe + SATA variants (197)
│   │       ├── battery.rs       #   starship-battery (28)
│   │       ├── ai_observability.rs  # token accounting on proxied traffic (403)
│   │       ├── cloud_ranges.rs  #   live AWS/GCP CIDR classify + fallback (429)
│   │       ├── containers.rs    #   docker CLI → bollard fallback (637)
│   │       ├── ebpf_egress.rs   #   aya userspace loader, opt-in (166)
│   │       ├── network_topology.rs # /proc + ss + lsof + Win FFI egress (762)
│   │       ├── external_baselines.rs # Arena ranks + git/GitHub DORA (448)
│   │       └── registry.rs      #   image-update digest compare (203)
│   ├── ebpf/                    #   separate aya-ebpf probe crate (opt-in feature)
│   ├── Cargo.toml               #   ebpf feature off by default; windows dep cfg-gated
│   ├── tauri.conf.json          #   CSP set, window 1280×800, loopback
│   └── capabilities/default.json#   permissions reduced to core:default
├── .github/workflows/ci.yml     #   clippy -D warnings + cargo test on push/PR
└── docs/                        #   design specs + this analysis
```

**Data flow.** The frontend polls the single Tauri command `get_stats` at
1 Hz. Fast metrics (CPU/mem/net/processes/sensors) are recomputed every poll;
slow/subprocess-spawning sources are served from per-category TTL caches
(`inventory` 5 min, `gpus` 30 s, `smart_disks` 60 s, `batteries` 30 s,
`egress` 5 s, `baselines` 60 s — `main.rs:49-54`). Background tasks
(`main.rs:433-477`) run the axum proxy, refresh cloud CIDR ranges every 12 h,
refresh baselines every 3 h, refresh containers every 20 s (each bounded by a
30 s timeout so a hung docker daemon can't stall the loop), and attempt the
eBPF probe load once at startup.

**Config surface.** No config file — the entire user-supplied surface is env
vars (`AETHERIS_OLLAMA_URL`, `AETHERIS_LMSTUDIO_URL`, `GITHUB_TOKEN`), launch
privilege (root/`CAP_BPF` for full egress + `smartctl`), and the working
directory (git-DORA + GitHub API key off the process CWD).

---

## 3. Plan → code verification

| Plan claim (PR) | Evidence in tree | Status |
|---|---|---|
| **#1** Loopback proxy `127.0.0.1:3030` (was `0.0.0.0`) | `main.rs:386`, `server/mod.rs` | ✅ |
| **#1** Restrictive CSP (was `null`) | `tauri.conf.json:23` | ✅ |
| **#1** Capabilities reduced | `capabilities/default.json` → `core:default` only | ✅ |
| **#1** Untrusted strings escaped | `main.js:21-25` `esc()` | ✅ |
| **#1** History-based RUL (SQLite) | `analytics/rul.rs`, `database.rs` | ✅ |
| **#2** Container telemetry (CLI→bollard) | `containers.rs` (637 ln), `bollard = "0.21"` | ✅ |
| **#2** Image-update detection | `registry.rs` (digest compare) | ✅ |
| **#3** eBPF per-PID egress (opt-in) | `ebpf_egress.rs` + `ebpf/` crate + `aya` optional dep + `[features] ebpf` | ✅ |
| **#4** CI-integrated DORA | `external_baselines.rs`, `.github/workflows/ci.yml` | ✅ |
| **#5** macOS/Windows egress | `network_topology.rs` (762 ln), `windows` dep `cfg(windows)`-gated, `plist` dep | ✅ |
| **#6** Instrument-deck frontend | `index.html`/`main.js`/`styles.css` + `docs/superpowers/specs/2026-07-09-instrument-deck-ui-design.md` | ✅ |
| **#7** Default window 1280×800 | `tauri.conf.json:13-20` (`minWidth 900`, `minHeight 600`, `center`) | ✅ |

Git history confirms all seven merges land on `master`; the working tree is
clean and the analysis branch is even with `master`.

---

## 4. Honesty invariant — spot-checked in code

The plan's central invariant ("never present fabricated data; missing →
`null`/`—`/`n/a`, never a fake `0`") is implemented, not just asserted:

- **`esc()`** on every untrusted string before render (`main.js:21`).
- **`fmtBytes()`** returns `—` for `null`/`NaN` but a real `0 B` for a true
  zero (`main.js:28-34`) — the distinction the invariant is about.
- **eBPF** degrades to a *silent no-op* without the feature or privilege:
  `snapshot()` returns `None`, `per_process_rollup()` returns an empty vec,
  and the accounting badge reports `"sockets"` instead of `"ebpf"`
  (`ebpf_egress.rs:33-49`, `main.rs:311`) — no synthesized bytes.
- **RUL** marks low-confidence projections `(est.)` (README §Predictive RUL).
- **Baselines/pricing** are labelled static reference data, and live SaaS
  pricing was *deliberately not built* to avoid fabricated precision.

---

## 5. Findings (gaps / nits)

1. **Backend literal-zero placeholder (minor honesty nit).**
   `get_stats` emits `"currentLoadUser": 0.0` and `"currentLoadSystem": 0.0`
   as hardcoded literals (`main.rs:364-365`). These are the user/system CPU
   split, which `sysinfo` doesn't provide here — so per the project's own
   invariant they should be `null` (rendered `—`), not `0.0`. The plan notes a
   *frontend* "CPU fabricating 0" fix in PR #6 review; this backend pair looks
   like the same class of issue, still present. Low impact (the UI may not
   surface these two fields), but it's the one place the backend ships a fake
   `0` rather than an honest unknown.

2. **`system`/`bios`/`baseboard` are `"N/A"` strings** (`main.rs:337-348`).
   These are honest-unknown (not fake data), but they're string `"N/A"` rather
   than JSON `null`; a `null` would be more consistent with the invariant and
   with how the frontend maps missing values to `—`.

3. **`operstate` hardcoded `"up"`** for every interface (`main.rs:244`), with
   an inline comment explaining sysinfo doesn't expose it. Cosmetic, honest
   about itself, but technically an assumed value.

4. **Not compile-verified in this environment.** A Tauri build needs
   WebKitGTK/GTK system libraries (the CI job installs `libwebkit2gtk-4.1-dev`
   et al.). This container doesn't have them, so this analysis is static only.
   CI is the source of truth for green build/test/clippy.

5. **eBPF `/proc/<pid>/comm` truncation.** Process names in the eBPF rollup
   come from `comm`, which the kernel truncates to 15 chars
   (`ebpf_egress.rs:43`). Accurate but abbreviated; the `ss`/`/proc` path has
   fuller names. Minor, already the documented trade-off.

None of these block use. Item 1 is the only one worth a one-line fix if strict
invariant conformance in the raw payload matters.

---

## 6. How to run (recap)

```bash
npm install
npm run dev          # dev; use `sudo -E` on Linux/macOS for SMART / full egress / DORA token
npm run build        # release
# True per-PID egress (Linux, nightly + bpf-linker, run as root):
#   cd src-tauri/ebpf && cargo fetch          # once, cold cache
#   cargo build --release --features ebpf      # then run the binary with sudo -E
```

See `scripts/setup-windows.ps1` (this branch) to scaffold the project under
`E:\Projects` on a Windows workstation in one command.
