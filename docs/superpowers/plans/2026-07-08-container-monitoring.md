# Container Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `extras.containers: []` stub with a real Docker container monitor (per-container health, resource usage, and upstream image-update status), surfaced in a dashboard panel.

**Architecture:** A background async task (`monitors::containers::refresh`) populates a global cache; the synchronous `get_stats` command reads it (same pattern as `cloud_ranges`/`external_baselines`). Collection auto-detects two backends — the `docker` CLI (primary) then bollard over the socket (fallback) — both normalizing to one `Container` shape. A separate `monitors::registry` module compares each image's local digest to the registry's current manifest digest for update detection.

**Tech Stack:** Rust, Tauri 2, `bollard = "0.21"`, `reqwest` (json), `serde`/`serde_json`, `tokio`. Frontend: vanilla JS + HTML in `src/`.

## Global Constraints

- **Never fabricate data.** Unknown/failed metrics serialize as `null`; unreachable Docker → `{status:"unavailable", reason, containers:[]}`; undeterminable image update → `imageUpdateAvailable: null`. (Copied from spec.)
- **bollard `0.21`** requires **tokio ^1.47**; project pins `tokio 1.52.3` (compatible). Confirm exact bollard 0.21 API names via `cargo doc -p bollard --open` / the compiler — the crate is newer than training data.
- **CPU% via `stream:false` (NOT `one_shot:true`)** so `precpu_stats` is populated. Formula: `(cpu_delta / system_delta) * online_cpus * 100` when both deltas > 0.
- **Registry checks cached 30–60 min per image** (Docker Hub anon limit is 100 pulls/6h); failures → `null`, keep last-known.
- All untrusted strings interpolated into `innerHTML` in `main.js` MUST pass through the existing `esc()` helper.
- After each task: `cargo test` green and `cargo clippy` warning-free before committing. One logical change per commit.
- Work on branch `feat/container-monitoring`. Rust crate root is `src-tauri/`.

## Normalized `Container` (produced by both backends, consumed by frontend)

Serialized camelCase JSON. Nullable metrics are `Option` → `null`.

```
id, name, image, state, status,
health          : "healthy"|"unhealthy"|"starting"|"none",
cpuPercent      : f64?,   memUsed/memLimit/memPercent : f64?,
netRx/netTx     : f64?,   blockRead/blockWrite        : f64?,
pids            : u64?,   restartCount                : u64?,
ports, createdAt, uptime : String,
imageUpdateAvailable : bool?    (null = unknown)
localDigest          : String?  (#[serde(skip)] — internal, for registry compare)
```

## File Structure

- Create `src-tauri/src/monitors/containers.rs` — `Container` struct, byte parsers, CLI backend, bollard backend, global cache, `refresh()`, `get_container_stats()`.
- Create `src-tauri/src/monitors/registry.rs` — image-ref parsing, `WWW-Authenticate` parsing, manifest-digest fetch + token, `check_update()`, per-image cache.
- Modify `src-tauri/src/monitors/mod.rs` — register both modules.
- Modify `src-tauri/Cargo.toml` — add `bollard = "0.21"`.
- Modify `src-tauri/src/main.rs` — `extras.containers` + spawn refresh loop.
- Modify `src/index.html` — Containers panel.
- Modify `src/main.js` — render containers.
- Modify `README.md` — move container monitoring from Roadmap → Implemented.

---

### Task 1: `Container` model + byte-string parsers

**Files:**
- Create: `src-tauri/src/monitors/containers.rs`
- Modify: `src-tauri/src/monitors/mod.rs`

**Interfaces:**
- Produces: `struct Container` (fields above); `fn parse_bytes(&str) -> Option<f64>`; `fn parse_pair(&str) -> (Option<f64>, Option<f64>)`; `fn parse_percent(&str) -> Option<f64>`.

- [ ] **Step 1: Register the module.** In `src-tauri/src/monitors/mod.rs` add after `pub mod cloud_ranges;`:

```rust
pub mod containers;
```

- [ ] **Step 2: Write `containers.rs` with the model + parsers + failing tests.**

```rust
//! Docker container monitor. See docs/superpowers/specs/2026-07-08-container-monitoring-design.md
use serde::Serialize;

#[derive(Serialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub health: String,
    pub cpu_percent: Option<f64>,
    pub mem_used: Option<f64>,
    pub mem_limit: Option<f64>,
    pub mem_percent: Option<f64>,
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
    pub block_read: Option<f64>,
    pub block_write: Option<f64>,
    pub pids: Option<u64>,
    pub restart_count: Option<u64>,
    pub ports: String,
    pub created_at: String,
    pub uptime: String,
    pub image_update_available: Option<bool>,
    #[serde(skip)]
    pub local_digest: Option<String>,
}

/// Parse a docker size string ("23.43MiB", "1.11MB", "401kB", "0B") to bytes.
/// Docker uses IEC (MiB/GiB, 1024) for memory and SI (kB/MB/GB, 1000) for I/O.
fn parse_bytes(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "--" || s == "N/A" {
        return None;
    }
    let idx = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(idx);
    let value: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        "KiB" => 1024f64,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        "PiB" => 1024f64.powi(5),
        _ => return None,
    };
    Some(value * mult)
}

/// Parse a docker "A / B" pair (MemUsage, NetIO, BlockIO) into (A, B) bytes.
fn parse_pair(s: &str) -> (Option<f64>, Option<f64>) {
    let mut it = s.split('/');
    let a = it.next().and_then(parse_bytes);
    let b = it.next().and_then(parse_bytes);
    (a, b)
}

/// Parse a "12.5%" percentage into f64.
fn parse_percent(s: &str) -> Option<f64> {
    s.trim().trim_end_matches('%').trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bytes_iec_and_si() {
        assert_eq!(parse_bytes("23.43MiB"), Some(23.43 * 1024.0 * 1024.0));
        assert_eq!(parse_bytes("1.11MB"), Some(1_110_000.0));
        assert_eq!(parse_bytes("401kB"), Some(401_000.0));
        assert_eq!(parse_bytes("0B"), Some(0.0));
        assert_eq!(parse_bytes("--"), None);
        assert_eq!(parse_bytes(""), None);
    }

    #[test]
    fn parses_pair_and_percent() {
        let (a, b) = parse_pair("1.11MB / 51.8MB");
        assert_eq!(a, Some(1_110_000.0));
        assert_eq!(b, Some(51_800_000.0));
        assert_eq!(parse_percent("0.10%"), Some(0.10));
        assert_eq!(parse_percent("12%"), Some(12.0));
        assert_eq!(parse_percent("--"), None);
    }
}
```

- [ ] **Step 3: Run tests — expect FAIL (compile) then PASS.**

Run: `cd src-tauri && cargo test --quiet containers 2>&1 | grep "test result"`
Expected: `test result: ok. 2 passed`

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/monitors/mod.rs src-tauri/src/monitors/containers.rs
git commit -m "feat(containers): Container model + docker byte-string parsers"
```

---

### Task 2: CLI backend — parse & merge `docker ps`/`stats`/`inspect`

**Files:**
- Modify: `src-tauri/src/monitors/containers.rs`

**Interfaces:**
- Consumes: `Container`, `parse_bytes`, `parse_pair`, `parse_percent` (Task 1).
- Produces: `fn merge_cli(ps: &str, stats: &str, inspect: &str) -> Vec<Container>` (pure; joins the three NDJSON/array docker outputs by short container id).

- [ ] **Step 1: Write the failing test with captured samples.** Add to the `tests` module in `containers.rs`:

```rust
    const PS: &str = r#"{"ID":"a67c38371df9","Names":"coolify-sentinel","Image":"ghcr.io/coollabsio/sentinel:0.0.21","State":"running","Status":"Up 20 hours (healthy)","HealthStatus":"healthy","RunningFor":"20 hours ago","CreatedAt":"2026-07-08 03:38:33 -0400 EDT","Ports":""}
{"ID":"b12cafe00042","Names":"flappy-db","Image":"postgres:16","State":"exited","Status":"Exited (1) 3 minutes ago","HealthStatus":"","RunningFor":"5 minutes ago","CreatedAt":"2026-07-08 01:00:00 -0400 EDT","Ports":""}"#;
    const STATS: &str = r#"{"ID":"a67c38371df9","Name":"coolify-sentinel","CPUPerc":"0.50%","MemUsage":"23.43MiB / 23.41GiB","MemPerc":"0.10%","NetIO":"1.11MB / 51.8MB","BlockIO":"32MB / 401kB","PIDs":"11"}"#;
    const INSPECT: &str = r#"[{"Id":"a67c38371df9abc","RestartCount":2,"Config":{"Image":"ghcr.io/coollabsio/sentinel:0.0.21"},"State":{"Health":{"Status":"healthy"}}},{"Id":"b12cafe00042abc","RestartCount":7,"Config":{"Image":"postgres:16"},"State":{}}]"#;

    #[test]
    fn merge_cli_joins_sources_by_id() {
        let c = merge_cli(PS, STATS, INSPECT);
        assert_eq!(c.len(), 2);
        let sentinel = c.iter().find(|x| x.name == "coolify-sentinel").unwrap();
        assert_eq!(sentinel.state, "running");
        assert_eq!(sentinel.health, "healthy");
        assert_eq!(sentinel.cpu_percent, Some(0.50));
        assert_eq!(sentinel.mem_used, Some(23.43 * 1024.0 * 1024.0));
        assert_eq!(sentinel.pids, Some(11));
        assert_eq!(sentinel.restart_count, Some(2));
        // Exited container: present, with stats null (no stats line), restart from inspect.
        let db = c.iter().find(|x| x.name == "flappy-db").unwrap();
        assert_eq!(db.state, "exited");
        assert_eq!(db.cpu_percent, None);
        assert_eq!(db.restart_count, Some(7));
        assert_eq!(db.health, "none");
    }
```

- [ ] **Step 2: Run — expect FAIL** (`merge_cli` not found).

Run: `cd src-tauri && cargo test --quiet merge_cli_joins 2>&1 | tail -5`
Expected: compile error `cannot find function merge_cli`.

- [ ] **Step 3: Implement `merge_cli` + helpers.** Add to `containers.rs` (above the `tests` module):

```rust
use serde_json::Value;
use std::collections::HashMap;

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn health_from(s: &str) -> String {
    match s {
        "healthy" | "unhealthy" | "starting" => s.to_string(),
        _ => "none".to_string(),
    }
}

/// Join `docker ps`/`stats` (newline-delimited JSON) and `docker inspect` (JSON
/// array) by 12-char container id into normalized Containers.
pub(crate) fn merge_cli(ps: &str, stats: &str, inspect: &str) -> Vec<Container> {
    // stats keyed by short id
    let mut stats_map: HashMap<String, Value> = HashMap::new();
    for line in stats.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(id) = v.get("ID").and_then(|x| x.as_str()) {
                stats_map.insert(short_id(id), v);
            }
        }
    }
    // inspect (array) keyed by short id -> (restart_count)
    let mut restart_map: HashMap<String, u64> = HashMap::new();
    if let Ok(arr) = serde_json::from_str::<Vec<Value>>(inspect) {
        for v in arr {
            if let Some(id) = v.get("Id").and_then(|x| x.as_str()) {
                if let Some(rc) = v.get("RestartCount").and_then(|x| x.as_u64()) {
                    restart_map.insert(short_id(id), rc);
                }
            }
        }
    }

    let mut out = Vec::new();
    for line in ps.lines().filter(|l| !l.trim().is_empty()) {
        let p: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = short_id(p.get("ID").and_then(|x| x.as_str()).unwrap_or(""));
        let get = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

        let mut c = Container {
            id: id.clone(),
            name: get("Names"),
            image: get("Image"),
            state: get("State"),
            status: get("Status"),
            health: health_from(&get("HealthStatus")),
            ports: get("Ports"),
            created_at: get("CreatedAt"),
            uptime: get("RunningFor"),
            restart_count: restart_map.get(&id).copied(),
            ..Default::default()
        };

        if let Some(s) = stats_map.get(&id) {
            let sg = |k: &str| s.get(k).and_then(|x| x.as_str()).unwrap_or("");
            c.cpu_percent = parse_percent(sg("CPUPerc"));
            let (mu, ml) = parse_pair(sg("MemUsage"));
            c.mem_used = mu;
            c.mem_limit = ml;
            c.mem_percent = parse_percent(sg("MemPerc"));
            let (rx, tx) = parse_pair(sg("NetIO"));
            c.net_rx = rx;
            c.net_tx = tx;
            let (br, bw) = parse_pair(sg("BlockIO"));
            c.block_read = br;
            c.block_write = bw;
            c.pids = sg("PIDs").trim().parse().ok();
        }
        out.push(c);
    }
    out
}
```

- [ ] **Step 4: Run — expect PASS.**

Run: `cd src-tauri && cargo test --quiet merge_cli_joins 2>&1 | grep "test result"`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/monitors/containers.rs
git commit -m "feat(containers): CLI backend parse+merge for ps/stats/inspect"
```

---

### Task 3: Global cache, async CLI collection, `get_container_stats`, `refresh`

**Files:**
- Modify: `src-tauri/src/monitors/containers.rs`

**Interfaces:**
- Consumes: `merge_cli` (Task 2).
- Produces: `pub fn get_container_stats() -> serde_json::Value`; `pub async fn refresh()`; `async fn collect_via_cli() -> Option<Vec<Container>>`.

- [ ] **Step 1: Write the failing test for the cache read.** Add to `tests`:

```rust
    #[test]
    fn get_container_stats_unavailable_before_refresh() {
        // With nothing cached, status is "unavailable" and containers is [].
        let v = get_container_stats();
        assert_eq!(v["status"], "unavailable");
        assert!(v["containers"].as_array().unwrap().is_empty());
    }
```

- [ ] **Step 2: Run — expect FAIL** (`get_container_stats` not found).

Run: `cd src-tauri && cargo test --quiet get_container_stats_unavailable 2>&1 | tail -3`
Expected: compile error.

- [ ] **Step 3: Implement cache + collection.** Add to `containers.rs`:

```rust
use serde_json::json;
use std::sync::RwLock;

struct Cache {
    status: &'static str,
    reason: String,
    containers: Vec<Container>,
}
static CACHE: RwLock<Option<Cache>> = RwLock::new(None);

/// Synchronous cache read for get_stats. Never blocks on Docker.
pub fn get_container_stats() -> Value {
    let guard = CACHE.read().unwrap();
    match guard.as_ref() {
        Some(c) => json!({
            "status": c.status,
            "reason": c.reason,
            "containers": c.containers,
        }),
        None => json!({ "status": "unavailable", "reason": "not collected yet", "containers": [] }),
    }
}

/// Run a docker CLI subcommand, returning stdout on success.
async fn docker(args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("docker").args(args).output().await.ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Collect containers via the docker CLI. None if docker is unavailable.
async fn collect_via_cli() -> Option<Vec<Container>> {
    let ps = docker(&["ps", "--format", "{{json .}}"]).await?;
    // Stats/inspect are best-effort enrichment; ps alone still yields containers.
    let stats = docker(&["stats", "--no-stream", "--format", "{{json .}}"]).await.unwrap_or_default();
    let ids: Vec<String> = ps
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("ID").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .collect();
    let inspect = if ids.is_empty() {
        "[]".to_string()
    } else {
        let mut args = vec!["inspect", "--format", "{{json .}}"];
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        args.extend(id_refs.iter());
        // `docker inspect --format {{json .}}` prints one JSON object per line, not an array;
        // wrap into an array for merge_cli.
        let raw = docker(&args).await.unwrap_or_default();
        let joined: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        format!("[{}]", joined.join(","))
    };
    Some(merge_cli(&ps, &stats, &inspect))
}

/// Refresh the cache. Tries CLI, then (Task 4) bollard, then records unavailable.
pub async fn refresh() {
    if let Some(containers) = collect_via_cli().await {
        *CACHE.write().unwrap() = Some(Cache {
            status: "ok",
            reason: String::new(),
            containers,
        });
        return;
    }
    // Backends exhausted (bollard added in Task 4 before this fallthrough).
    *CACHE.write().unwrap() = Some(Cache {
        status: "unavailable",
        reason: "docker CLI and socket both unavailable".to_string(),
        containers: Vec::new(),
    });
}
```

- [ ] **Step 4: Run — expect PASS** (test runs before any refresh, so cache is empty).

Run: `cd src-tauri && cargo test --quiet get_container_stats_unavailable 2>&1 | grep "test result"`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Add a live probe and run it against real containers.** Add to `tests`:

```rust
    #[tokio::test]
    #[ignore = "live: requires docker with running containers on this host"]
    async fn live_cli_probe() {
        let c = collect_via_cli().await.expect("docker CLI should work on this host");
        eprintln!("collected {} containers via CLI", c.len());
        for x in c.iter().take(5) {
            eprintln!("{} | {} | {} | cpu={:?} mem={:?} restarts={:?}",
                x.name, x.state, x.health, x.cpu_percent, x.mem_used, x.restart_count);
        }
        assert!(!c.is_empty());
    }
```

Run: `cd src-tauri && cargo test --quiet live_cli_probe -- --ignored --nocapture 2>&1 | grep -E "collected|test result"`
Expected: `collected 26 containers via CLI` (approx) and `test result: ok. 1 passed`. If it fails to compile because `tokio::test` needs the macro, confirm `[dev-dependencies] tokio` has `features=["macros","rt-multi-thread"]` (already present from earlier work).

- [ ] **Step 6: Commit.**

```bash
git add src-tauri/src/monitors/containers.rs
git commit -m "feat(containers): global cache + async CLI collection + get_container_stats"
```

---

### Task 4: bollard fallback backend

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/monitors/containers.rs`

**Interfaces:**
- Consumes: `Container`, `merge_cli` output shape.
- Produces: `async fn collect_via_bollard() -> Option<Vec<Container>>`; `fn cpu_percent(cpu_total, precpu_total, system, presystem, online_cpus) -> Option<f64>` (pure).

- [ ] **Step 1: Add the dependency.** In `src-tauri/Cargo.toml` under `[dependencies]`:

```toml
bollard = "0.21"
```

Run: `cd src-tauri && cargo check --quiet 2>&1 | grep -E "^error" | head`
Expected: no errors (dependency resolves/builds).

- [ ] **Step 2: Write the failing CPU%-formula test.** Add to `tests`:

```rust
    #[test]
    fn cpu_percent_formula() {
        // cpu_delta=100, system_delta=1000, 4 cpus -> 100/1000*4*100 = 40%
        assert_eq!(cpu_percent(200, 100, 5000, 4000, 4), Some(40.0));
        // zero system delta -> None (no divide-by-zero, no fake 0)
        assert_eq!(cpu_percent(200, 100, 4000, 4000, 4), None);
    }
```

- [ ] **Step 3: Run — expect FAIL** (`cpu_percent` not found).

Run: `cd src-tauri && cargo test --quiet cpu_percent_formula 2>&1 | tail -3`
Expected: compile error.

- [ ] **Step 4: Implement the formula + bollard backend.** Add to `containers.rs`.

The pure formula (define first — this is what the test needs):

```rust
fn cpu_percent(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online_cpus: u64) -> Option<f64> {
    let cpu_delta = cpu_total.checked_sub(precpu_total)? as f64;
    let system_delta = system.checked_sub(presystem)? as f64;
    if system_delta <= 0.0 || cpu_delta < 0.0 {
        return None;
    }
    let cpus = if online_cpus == 0 { 1 } else { online_cpus } as f64;
    Some(cpu_delta / system_delta * cpus * 100.0)
}
```

The bollard backend (CONFIRM exact 0.21 API names via `cargo doc -p bollard`; adjust field paths the compiler flags — the shape below matches bollard's Docker Engine models):

```rust
async fn collect_via_bollard() -> Option<Vec<Container>> {
    use bollard::Docker;
    let docker = Docker::connect_with_local_defaults().ok()?;
    // Verify the daemon answers before committing to this backend.
    docker.ping().await.ok()?;

    let summaries = docker
        .list_containers::<String>(Some(bollard::container::ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
        .ok()?;

    let mut out = Vec::new();
    for s in summaries {
        let id = s.id.clone().unwrap_or_default();
        let sid = short_id(&id);
        let mut c = Container {
            id: sid,
            name: s.names.as_ref().and_then(|n| n.first()).map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default(),
            image: s.image.clone().unwrap_or_default(),
            state: s.state.clone().unwrap_or_default(),
            status: s.status.clone().unwrap_or_default(),
            health: "none".to_string(),
            ..Default::default()
        };

        // Restart count + health via inspect.
        if let Ok(insp) = docker.inspect_container(&id, None).await {
            if let Some(st) = insp.state.as_ref() {
                c.restart_count = insp.restart_count.map(|r| r as u64);
                if let Some(h) = st.health.as_ref().and_then(|h| h.status.as_ref()) {
                    c.health = health_from(&format!("{h:?}").to_lowercase());
                }
            }
        }

        // One-shot-but-accurate stats (stream:false gives populated precpu_stats).
        if let Ok(mut stream) = std::panic::AssertUnwindSafe(async {
            use futures_util::StreamExt;
            docker.stats(&id, Some(bollard::container::StatsOptions { stream: false, one_shot: false })).next().await
        }).await {
            if let Some(Ok(st)) = stream {
                let cpu = st.cpu_stats;
                let pre = st.precpu_stats;
                c.cpu_percent = cpu_percent(
                    cpu.cpu_usage.total_usage,
                    pre.cpu_usage.total_usage,
                    cpu.system_cpu_usage.unwrap_or(0),
                    pre.system_cpu_usage.unwrap_or(0),
                    cpu.online_cpus.unwrap_or(0),
                );
                if let Some(mem) = st.memory_stats.usage { c.mem_used = Some(mem as f64); }
                if let Some(lim) = st.memory_stats.limit { c.mem_limit = Some(lim as f64); }
                if let (Some(u), Some(l)) = (c.mem_used, c.mem_limit) {
                    if l > 0.0 { c.mem_percent = Some(u / l * 100.0); }
                }
                if let Some(nets) = st.networks {
                    c.net_rx = Some(nets.values().map(|n| n.rx_bytes as f64).sum());
                    c.net_tx = Some(nets.values().map(|n| n.tx_bytes as f64).sum());
                }
            }
        }
        out.push(c);
    }
    Some(out)
}
```

Then wire it into `refresh()` — replace the fallthrough body so it tries bollard before recording unavailable:

```rust
pub async fn refresh() {
    let collected = match collect_via_cli().await {
        Some(c) => Some(c),
        None => collect_via_bollard().await,
    };
    match collected {
        Some(containers) => {
            *CACHE.write().unwrap() = Some(Cache { status: "ok", reason: String::new(), containers });
        }
        None => {
            *CACHE.write().unwrap() = Some(Cache {
                status: "unavailable",
                reason: "docker CLI and socket both unavailable".to_string(),
                containers: Vec::new(),
            });
        }
    }
}
```

- [ ] **Step 5: Run the formula test + `cargo check`.**

Run: `cd src-tauri && cargo test --quiet cpu_percent_formula 2>&1 | grep "test result" && cargo clippy --quiet 2>&1 | grep -c warning`
Expected: `test result: ok. 2 passed` and `0`. Fix any bollard API mismatches the compiler reports (method/field names) before proceeding.

- [ ] **Step 6: Commit.**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/monitors/containers.rs
git commit -m "feat(containers): bollard socket fallback backend + CPU% formula"
```

---

### Task 5: Registry — image-ref & `WWW-Authenticate` parsing

**Files:**
- Create: `src-tauri/src/monitors/registry.rs`
- Modify: `src-tauri/src/monitors/mod.rs`

**Interfaces:**
- Produces: `struct ImageRef { registry, repository, tag }`; `fn parse_image_ref(&str) -> Option<ImageRef>`; `fn parse_www_authenticate(&str) -> Option<(String realm, String service, Option<String> scope)>`.

- [ ] **Step 1: Register module.** In `mod.rs` add `pub mod registry;`.

- [ ] **Step 2: Write `registry.rs` with parsers + failing tests.**

```rust
//! Registry v2 image-update detection via manifest-digest comparison.

#[derive(Debug, PartialEq)]
pub struct ImageRef {
    pub registry: String,   // host used for the v2 API, e.g. registry-1.docker.io
    pub repository: String, // e.g. library/postgres, coollabsio/sentinel
    pub tag: String,
}

/// Parse a docker image reference into (registry, repository, tag), applying
/// Docker Hub defaulting (implicit docker.io + library/ for single-segment names).
pub fn parse_image_ref(image: &str) -> Option<ImageRef> {
    if image.is_empty() || image.contains('@') && !image.contains(':') {
        // digest-only refs without a tag aren't checkable here
    }
    let (name, tag) = match image.rsplit_once(':') {
        // A ':' after the last '/' is the tag; a ':' before a '/' is a port.
        Some((n, t)) if !t.contains('/') => (n, t.to_string()),
        _ => (image, "latest".to_string()),
    };
    let first = name.split('/').next().unwrap_or("");
    let is_registry = first.contains('.') || first.contains(':') || first == "localhost";
    let (registry, repository) = if is_registry {
        let (host, repo) = name.split_once('/')?;
        let host = if host == "docker.io" { "registry-1.docker.io".to_string() } else { host.to_string() };
        (host, repo.to_string())
    } else if name.contains('/') {
        ("registry-1.docker.io".to_string(), name.to_string())
    } else {
        ("registry-1.docker.io".to_string(), format!("library/{name}"))
    };
    Some(ImageRef { registry, repository, tag })
}

/// Parse a Bearer `WWW-Authenticate` challenge into (realm, service, scope).
pub fn parse_www_authenticate(header: &str) -> Option<(String, String, Option<String>)> {
    let h = header.trim();
    let rest = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in rest.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "realm" => realm = Some(v),
            "service" => service = Some(v),
            "scope" => scope = Some(v),
            _ => {}
        }
    }
    Some((realm?, service?, scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_refs() {
        assert_eq!(parse_image_ref("postgres:16").unwrap(),
            ImageRef { registry: "registry-1.docker.io".into(), repository: "library/postgres".into(), tag: "16".into() });
        assert_eq!(parse_image_ref("ghcr.io/coollabsio/sentinel:0.0.21").unwrap(),
            ImageRef { registry: "ghcr.io".into(), repository: "coollabsio/sentinel".into(), tag: "0.0.21".into() });
        assert_eq!(parse_image_ref("crazymax/diun").unwrap(),
            ImageRef { registry: "registry-1.docker.io".into(), repository: "crazymax/diun".into(), tag: "latest".into() });
        // registry with port is not mistaken for a tag
        assert_eq!(parse_image_ref("localhost:5000/app:v1").unwrap(),
            ImageRef { registry: "localhost:5000".into(), repository: "app".into(), tag: "v1".into() });
    }

    #[test]
    fn parses_challenge() {
        let h = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/postgres:pull""#;
        let (realm, service, scope) = parse_www_authenticate(h).unwrap();
        assert_eq!(realm, "https://auth.docker.io/token");
        assert_eq!(service, "registry.docker.io");
        assert_eq!(scope.unwrap(), "repository:library/postgres:pull");
    }
}
```

- [ ] **Step 3: Run — expect PASS after implementing (write test first, it fails to compile, then the impl above makes it pass).**

Run: `cd src-tauri && cargo test --quiet registry 2>&1 | grep "test result"`
Expected: `test result: ok. 2 passed`

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/monitors/mod.rs src-tauri/src/monitors/registry.rs
git commit -m "feat(registry): image-ref and WWW-Authenticate parsing"
```

---

### Task 6: Registry — async digest fetch, token dance, per-image cache

**Files:**
- Modify: `src-tauri/src/monitors/registry.rs`

**Interfaces:**
- Consumes: `parse_image_ref`, `parse_www_authenticate` (Task 5).
- Produces: `pub async fn check_update(client: &reqwest::Client, image: &str, local_digest: Option<&str>) -> Option<bool>` (None = undeterminable). Uses an internal per-image cache with a 45-min TTL.

- [ ] **Step 1: Implement the fetch + cache.** Add to `registry.rs`:

```rust
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";
const TTL: Duration = Duration::from_secs(45 * 60);

struct Cached { at: Instant, result: Option<bool> }
static CACHE: RwLock<Option<HashMap<String, Cached>>> = RwLock::new(None);

/// Compare the running image's local digest to the registry's current tag digest.
/// Returns Some(true/false) only on a successful compare; None when unknown
/// (no local digest, private/unauthorized, network/registry error).
pub async fn check_update(client: &reqwest::Client, image: &str, local_digest: Option<&str>) -> Option<bool> {
    let local = local_digest?; // no local repo digest -> undeterminable
    // cache hit?
    {
        let g = CACHE.read().unwrap();
        if let Some(m) = g.as_ref() {
            if let Some(c) = m.get(image) {
                if c.at.elapsed() < TTL {
                    return c.result;
                }
            }
        }
    }
    let result = fetch_remote_digest(client, image).await.map(|remote| remote != local);
    let mut g = CACHE.write().unwrap();
    g.get_or_insert_with(HashMap::new)
        .insert(image.to_string(), Cached { at: Instant::now(), result });
    result
}

async fn fetch_remote_digest(client: &reqwest::Client, image: &str) -> Option<String> {
    let r = parse_image_ref(image)?;
    let url = format!("https://{}/v2/{}/manifests/{}", r.registry, r.repository, r.tag);
    let head = |token: Option<&str>| {
        let mut req = client.head(&url).header("Accept", ACCEPT);
        if let Some(t) = token { req = req.bearer_auth(t); }
        req
    };
    let resp = head(None).send().await.ok()?;
    let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let chal = resp.headers().get("www-authenticate")?.to_str().ok()?.to_string();
        let (realm, service, scope) = parse_www_authenticate(&chal)?;
        let scope = scope.unwrap_or_else(|| format!("repository:{}:pull", r.repository));
        let token: serde_json::Value = client
            .get(&realm).query(&[("service", service.as_str()), ("scope", scope.as_str())])
            .send().await.ok()?.json().await.ok()?;
        let tok = token.get("token").or_else(|| token.get("access_token")).and_then(|t| t.as_str())?;
        head(Some(tok)).send().await.ok()?
    } else {
        resp
    };
    if !resp.status().is_success() { return None; }
    resp.headers().get("docker-content-digest")?.to_str().ok().map(|s| s.to_string())
}
```

- [ ] **Step 2: Add a challenge-cache unit test.** Add to `tests`:

```rust
    #[tokio::test]
    async fn check_update_none_without_local_digest() {
        let client = reqwest::Client::new();
        assert_eq!(check_update(&client, "postgres:16", None).await, None);
    }
```

Run: `cd src-tauri && cargo test --quiet check_update_none 2>&1 | grep "test result"`
Expected: `test result: ok. 1 passed`

- [ ] **Step 3: Add a live probe against a public image.** Add to `tests`:

```rust
    #[tokio::test]
    #[ignore = "live: hits ghcr.io anonymously"]
    async fn live_registry_probe() {
        let client = reqwest::Client::new();
        // A digest that cannot match the current one -> Some(true); a wrong-but-present flow proves the fetch+token path.
        let r = check_update(&client, "ghcr.io/coollabsio/sentinel:0.0.21", Some("sha256:0000000000000000000000000000000000000000000000000000000000000000")).await;
        eprintln!("registry check result: {r:?}");
        assert_eq!(r, Some(true), "a bogus local digest must differ from the real remote digest");
    }
```

Run: `cd src-tauri && cargo test --quiet live_registry_probe -- --ignored --nocapture 2>&1 | grep -E "registry check|test result"`
Expected: `registry check result: Some(true)` and `test result: ok. 1 passed`.

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/monitors/registry.rs
git commit -m "feat(registry): manifest-digest fetch with token auth + per-image cache"
```

---

### Task 7: Wire registry update checks into container refresh

**Files:**
- Modify: `src-tauri/src/monitors/containers.rs`

**Interfaces:**
- Consumes: `registry::check_update` (Task 6); `Container.local_digest`, `Container.image`.
- Produces: containers with `image_update_available` populated; `local_digest` filled by both backends.

- [ ] **Step 1: Populate `local_digest` in both backends.**
  - CLI: after building the container list in `collect_via_cli`, fetch repo digests once via `docker image inspect --format '{{json .}}' <unique images>` and attach. Add before `Some(merge_cli(...))` return — collect merged first, then enrich:

```rust
    let mut containers = merge_cli(&ps, &stats, &inspect);
    // Local repo digests, keyed by image ref, via one batched image inspect.
    let images: Vec<String> = {
        let mut v: Vec<String> = containers.iter().map(|c| c.image.clone()).collect();
        v.sort(); v.dedup(); v
    };
    if !images.is_empty() {
        let mut args = vec!["image", "inspect", "--format", "{{json .}}"];
        let refs: Vec<&str> = images.iter().map(|s| s.as_str()).collect();
        args.extend(refs.iter());
        if let Some(raw) = docker(&args).await {
            let mut digest_by_ref: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for line in raw.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    let rds = v.get("RepoDigests").and_then(|x| x.as_array());
                    let tags = v.get("RepoTags").and_then(|x| x.as_array());
                    if let (Some(rds), Some(tags)) = (rds, tags) {
                        // Map each RepoTag to the (first) RepoDigest's digest part.
                        if let Some(dig) = rds.iter().filter_map(|d| d.as_str()).find_map(|d| d.split_once('@').map(|(_, h)| h.to_string())) {
                            for t in tags.iter().filter_map(|t| t.as_str()) {
                                digest_by_ref.insert(t.to_string(), dig.clone());
                            }
                        }
                    }
                }
            }
            for c in &mut containers {
                c.local_digest = digest_by_ref.get(&c.image).cloned();
            }
        }
    }
    Some(containers)
```

  - bollard: in `collect_via_bollard`, after building each `c`, fetch its image digest:

```rust
        if let Ok(img) = docker.inspect_image(&c.image).await {
            c.local_digest = img.repo_digests.as_ref()
                .and_then(|v| v.first())
                .and_then(|d| d.split_once('@').map(|(_, h)| h.to_string()));
        }
```

- [ ] **Step 2: Run the update checks concurrently in `refresh`.** Modify `refresh()` so, after collecting containers, it fills `image_update_available`:

```rust
pub async fn refresh() {
    let collected = match collect_via_cli().await {
        Some(c) => Some(c),
        None => collect_via_bollard().await,
    };
    let mut containers = match collected {
        Some(c) => c,
        None => {
            *CACHE.write().unwrap() = Some(Cache {
                status: "unavailable",
                reason: "docker CLI and socket both unavailable".to_string(),
                containers: Vec::new(),
            });
            return;
        }
    };
    let client = reqwest::Client::new();
    let checks = futures_util::future::join_all(containers.iter().map(|c| {
        let image = c.image.clone();
        let digest = c.local_digest.clone();
        let client = &client;
        async move { crate::monitors::registry::check_update(client, &image, digest.as_deref()).await }
    })).await;
    for (c, upd) in containers.iter_mut().zip(checks) {
        c.image_update_available = upd;
    }
    *CACHE.write().unwrap() = Some(Cache { status: "ok", reason: String::new(), containers });
}
```

- [ ] **Step 3: Verify build + full live probe.**

Run: `cd src-tauri && cargo clippy --quiet 2>&1 | grep -c warning && cargo test --quiet live_cli_probe -- --ignored --nocapture 2>&1 | grep -E "collected|test result"`
Expected: `0` warnings; probe still collects the containers. (Update flags are exercised end-to-end in Task 10.)

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/monitors/containers.rs
git commit -m "feat(containers): populate local digest + concurrent image-update checks"
```

---

### Task 8: Wire into `get_stats` + background refresh

**Files:**
- Modify: `src-tauri/src/main.rs`

**Interfaces:**
- Consumes: `monitors::containers::{get_container_stats, refresh}`.

- [ ] **Step 1: Replace the stub in `get_stats`.** In `src-tauri/src/main.rs`, in the `extras` object, replace `"containers": [],` with:

```rust
                "containers": monitors::containers::get_container_stats(),
```

- [ ] **Step 2: Spawn the refresh loop.** In the `.setup(...)` closure, after the existing `external_baselines` spawn, add:

```rust
            tauri::async_runtime::spawn(async move {
                loop {
                    monitors::containers::refresh().await;
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                }
            });
```

- [ ] **Step 3: Verify build.**

Run: `cd src-tauri && cargo check --quiet 2>&1 | grep -E "^error" | head; cargo clippy --quiet 2>&1 | grep -c warning`
Expected: no errors; `0` warnings.

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/main.rs
git commit -m "feat(containers): surface containers in get_stats + background refresh"
```

---

### Task 9: Frontend Containers panel

**Files:**
- Modify: `src/index.html`, `src/main.js`

**Interfaces:**
- Consumes: `stats.dynamic.extras.containers` = `{status, reason, containers:[Container]}`.

- [ ] **Step 1: Add the panel markup.** In `src/index.html`, after the Egress Topology `</section>` (before the External Baselines panel), add:

```html
        <!-- Containers Panel -->
        <section class="glass-panel card col-span-2">
          <h2>Containers</h2>
          <div class="table-container scrollable">
            <table>
              <thead>
                <tr><th>Name / Image</th><th>State</th><th>CPU%</th><th>Mem</th><th>Net I/O</th><th>Restarts</th><th>Uptime</th><th>Update</th></tr>
              </thead>
              <tbody id="containers-body">
                <tr><td colspan="8" class="empty-state">Scanning containers…</td></tr>
              </tbody>
            </table>
          </div>
        </section>
```

- [ ] **Step 2: Render it in `main.js`.** Add inside `updateDOM()` (after the egress block):

```javascript
        // --- Containers ---
        const cw = (stats.dynamic.extras && stats.dynamic.extras.containers) || {};
        let contHtml = '';
        if (cw.status === 'ok' && cw.containers && cw.containers.length > 0) {
            cw.containers.forEach(c => {
                const healthColor = c.health === 'unhealthy' ? 'var(--danger)'
                    : c.state !== 'running' ? 'var(--danger)'
                    : c.health === 'starting' ? 'var(--warning)' : 'inherit';
                const restartColor = (c.restartCount || 0) > 3 ? 'var(--warning)' : 'inherit';
                const cpu = c.cpuPercent != null ? `${c.cpuPercent.toFixed(1)}%` : '--';
                const mem = c.memUsed != null ? `${formatBytes(c.memUsed)}${c.memLimit ? ' / ' + formatBytes(c.memLimit) : ''}` : '--';
                const net = (c.netRx != null || c.netTx != null) ? `↓${formatBytes(c.netRx || 0)} ↑${formatBytes(c.netTx || 0)}` : '--';
                const upd = c.imageUpdateAvailable === true ? '⬆'
                    : c.imageUpdateAvailable === false ? '' : '?';
                contHtml += `
                    <tr>
                        <td>${esc(c.name)}<br><span class="label" style="font-size:0.7rem">${esc(c.image)}</span></td>
                        <td style="color:${healthColor}">${esc(c.state)}${c.health !== 'none' ? ' · ' + esc(c.health) : ''}</td>
                        <td>${cpu}</td>
                        <td>${mem}</td>
                        <td><span style="font-size:0.75rem">${net}</span></td>
                        <td style="color:${restartColor}">${c.restartCount != null ? c.restartCount : '--'}</td>
                        <td><span class="label" style="font-size:0.75rem">${esc(c.uptime)}</span></td>
                        <td title="image update available">${upd}</td>
                    </tr>`;
            });
        } else {
            const reason = cw.reason ? ` (${esc(cw.reason)})` : '';
            contHtml = `<tr><td colspan="8" class="empty-state">Docker not available${reason}</td></tr>`;
        }
        const contBody = document.getElementById('containers-body');
        if (contBody) contBody.innerHTML = contHtml;
```

- [ ] **Step 3: Verify HTML well-formedness.**

Run:
```bash
cd /home/ubuntu/.gemini/antigravity/scratch/aetheris-rs
python3 -c "h=open('src/index.html').read(); print('sections balanced:', h.count('<section')==h.count('</section>'))"
```
Expected: `sections balanced: True`

- [ ] **Step 4: Commit.**

```bash
git add src/index.html src/main.js
git commit -m "feat(ui): containers panel with health/restart cues and update indicator"
```

---

### Task 10: End-to-end verification + README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Full end-to-end live probe.** Add to `containers.rs` `tests`:

```rust
    #[tokio::test]
    #[ignore = "live: full refresh incl. registry checks against real containers"]
    async fn live_full_refresh() {
        refresh().await;
        let v = get_container_stats();
        eprintln!("status={} count={}", v["status"], v["containers"].as_array().unwrap().len());
        let arr = v["containers"].as_array().unwrap();
        assert_eq!(v["status"], "ok");
        assert!(!arr.is_empty());
        // Structural honesty: metrics are numbers or null, never fabricated strings.
        for c in arr {
            assert!(c["cpuPercent"].is_number() || c["cpuPercent"].is_null());
            assert!(c["imageUpdateAvailable"].is_boolean() || c["imageUpdateAvailable"].is_null());
        }
        let updates = arr.iter().filter(|c| c["imageUpdateAvailable"] == serde_json::Value::Bool(true)).count();
        eprintln!("containers with image updates available: {updates}");
    }
```

Run: `cd src-tauri && cargo test --quiet live_full_refresh -- --ignored --nocapture 2>&1 | grep -E "status=|updates|test result"`
Expected: `status="ok" count=~26`, an updates count printed, `test result: ok. 1 passed`.

- [ ] **Step 2: Full suite + clippy.**

Run: `cd src-tauri && cargo test --quiet 2>&1 | grep "test result" | head -1 && cargo clippy --quiet 2>&1 | grep -c warning`
Expected: all tests pass; `0` warnings.

- [ ] **Step 3: Update README.** In `README.md`, remove the "Container-level monitoring" bullet from the Roadmap section and add under Implemented (after External baselines):

```markdown
### Containers (Linux/any Docker host)
- Real per-container telemetry via the Docker CLI (auto-falling back to the
  Engine API over the socket via bollard): name, image, state, health, CPU%,
  memory, network/block I/O, PIDs, restart count, ports, uptime.
- **Image-update detection**: compares each image's local digest to the
  registry's current manifest digest (anonymous bearer-token auth), cached per
  image ~45 min. `true`/`false` only on a real compare; `null` when unknown.
```

Also update the platform-support matrix to add a "Containers ✅/✅/✅" row.

- [ ] **Step 4: Commit.**

```bash
git add README.md src-tauri/src/monitors/containers.rs
git commit -m "docs: mark container monitoring implemented; end-to-end live verification"
```

---

## Self-Review

**Spec coverage:** goal (Tasks 3/8/9) ✓; non-goals (read-only — no control commands added) ✓; auto-detect CLI→bollard (Tasks 3–4, refresh) ✓; data model incl. all fields (Tasks 1–2, 4) ✓; image-update via registry digest + token + cache (Tasks 5–7) ✓; caching cadences (Task 8 loop = 20s; registry 45min = Task 6 TTL) ✓; frontend panel + honest empty state (Task 9) ✓; error/empty states (Task 3 unavailable, `Option`→null throughout) ✓; testing incl. pure parsers + live probes (every task) ✓; deps `bollard 0.21` (Task 4) ✓; README (Task 10) ✓.

**Placeholder scan:** no TBD/TODO; every code step shows complete code. The one external-API caveat (bollard 0.21 exact field paths) has an explicit "confirm via cargo doc/compiler" verification step in Task 4 — the compiler is ground truth for a crate newer than training data.

**Type consistency:** `Container` fields (snake_case Rust → camelCase JSON) referenced consistently; `merge_cli`, `collect_via_cli`, `collect_via_bollard`, `cpu_percent`, `check_update`, `parse_image_ref`, `parse_www_authenticate`, `get_container_stats`, `refresh` names match across tasks. Frontend reads the same camelCase keys the struct serializes.
