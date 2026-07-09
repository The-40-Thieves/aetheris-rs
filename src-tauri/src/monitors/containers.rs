//! Docker container monitor. See docs/superpowers/specs/2026-07-08-container-monitoring-design.md
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::RwLock;

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
    let mut containers = merge_cli(&ps, &stats, &inspect);
    // Local repo digests, keyed by image ref, via one batched image inspect.
    let images: Vec<String> = {
        let mut v: Vec<String> = containers.iter().map(|c| c.image.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    if !images.is_empty() {
        let mut args = vec!["image", "inspect", "--format", "{{json .}}"];
        let refs: Vec<&str> = images.iter().map(|s| s.as_str()).collect();
        args.extend(refs.iter());
        if let Some(raw) = docker(&args).await {
            let mut digest_by_ref: HashMap<String, String> = HashMap::new();
            for line in raw.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    let rds = v.get("RepoDigests").and_then(|x| x.as_array());
                    let tags = v.get("RepoTags").and_then(|x| x.as_array());
                    if let (Some(rds), Some(tags)) = (rds, tags) {
                        // Map each RepoTag to the (first) RepoDigest's digest part.
                        if let Some(dig) = rds
                            .iter()
                            .filter_map(|d| d.as_str())
                            .find_map(|d| d.split_once('@').map(|(_, h)| h.to_string()))
                        {
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
}

/// Docker's CPU% formula: (cpu_delta / system_delta) * online_cpus * 100.
/// None on any missing/non-monotonic sample (never fabricates a 0%).
fn cpu_percent(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online_cpus: u64) -> Option<f64> {
    let cpu_delta = cpu_total.checked_sub(precpu_total)? as f64;
    let system_delta = system.checked_sub(presystem)? as f64;
    if system_delta <= 0.0 {
        return None;
    }
    let cpus = if online_cpus == 0 { 1 } else { online_cpus } as f64;
    Some(cpu_delta / system_delta * cpus * 100.0)
}

/// Format a single port mapping the way `docker ps` does, e.g.
/// "0.0.0.0:8080->80/tcp" (published) or "80/tcp" (unpublished).
fn format_port(p: &bollard::models::PortSummary) -> String {
    let typ = p.typ.map(|t| t.to_string()).unwrap_or_default();
    match (p.ip.as_deref(), p.public_port) {
        (Some(ip), Some(pub_port)) => format!("{ip}:{pub_port}->{}/{typ}", p.private_port),
        (None, Some(pub_port)) => format!("{pub_port}->{}/{typ}", p.private_port),
        _ => format!("{}/{typ}", p.private_port),
    }
}

/// A simple, honest relative-time string ("3h 20m", "5d") — need not byte-match
/// docker's exact wording.
fn format_uptime(secs: i64) -> String {
    let secs = secs.max(0) as u64;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{secs}s")
    }
}

/// Collect containers via the bollard Docker Engine API (socket), used when
/// the `docker` CLI itself is unavailable but the daemon socket is reachable.
/// None if the daemon can't be reached at all.
async fn collect_via_bollard() -> Option<Vec<Container>> {
    use bollard::query_parameters::{InspectContainerOptions, ListContainersOptions, StatsOptions};
    use bollard::Docker;
    use futures_util::StreamExt;

    let docker = Docker::connect_with_local_defaults().ok()?;
    // Verify the daemon answers before committing to this backend.
    docker.ping().await.ok()?;

    let summaries = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
        .ok()?;

    let mut out = Vec::new();
    for s in summaries {
        let id = s.id.clone().unwrap_or_default();
        let sid = short_id(&id);
        let ports = s
            .ports
            .as_ref()
            .map(|ps| ps.iter().map(format_port).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let created_at = s
            .created
            .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();

        let mut c = Container {
            id: sid,
            name: s
                .names
                .as_ref()
                .and_then(|n| n.first())
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default(),
            image: s.image.clone().unwrap_or_default(),
            state: s.state.map(|st| st.as_ref().to_string()).unwrap_or_default(),
            status: s.status.clone().unwrap_or_default(),
            health: "none".to_string(),
            ports,
            created_at,
            ..Default::default()
        };

        // Relative uptime only makes sense for a currently-running container;
        // never fabricate one for a stopped/exited container.
        c.uptime = if c.state == "running" {
            s.created
                .map(|created_secs| format_uptime(chrono::Utc::now().timestamp().saturating_sub(created_secs)))
                .unwrap_or_default()
        } else {
            String::new()
        };

        // Restart count + health via inspect.
        if let Ok(insp) = docker
            .inspect_container(&id, None::<InspectContainerOptions>)
            .await
        {
            c.restart_count = insp.restart_count.map(|r| r as u64);
            if let Some(h) = insp
                .state
                .as_ref()
                .and_then(|st| st.health.as_ref())
                .and_then(|h| h.status)
            {
                c.health = health_from(h.as_ref());
            }
        }

        // One-shot-but-accurate stats (stream:false gives populated precpu_stats).
        // `stats()` returns a Stream directly (not a Future); one `next()` yields
        // a single sample when stream:false is set.
        let mut stream = docker.stats(
            &id,
            Some(StatsOptions {
                stream: false,
                one_shot: false,
            }),
        );
        if let Some(Ok(st)) = stream.next().await {
            if let (Some(cpu), Some(pre)) = (st.cpu_stats.as_ref(), st.precpu_stats.as_ref()) {
                let cpu_usage = cpu.cpu_usage.as_ref();
                let precpu_usage = pre.cpu_usage.as_ref();
                let cpu_total = cpu_usage.and_then(|u| u.total_usage);
                let precpu_total = precpu_usage.and_then(|u| u.total_usage);
                let system = cpu.system_cpu_usage;
                let presystem = pre.system_cpu_usage;
                // Only compute cpu_percent when every input sample is actually
                // present; never fabricate a 0% from a missing field.
                if let (Some(cpu_total), Some(precpu_total), Some(system), Some(presystem)) =
                    (cpu_total, precpu_total, system, presystem)
                {
                    let online_cpus = cpu.online_cpus.map(|n| n as u64).unwrap_or_else(|| {
                        cpu_usage
                            .and_then(|u| u.percpu_usage.as_ref())
                            .map(|v| v.len() as u64)
                            .unwrap_or(1)
                    });
                    c.cpu_percent = cpu_percent(cpu_total, precpu_total, system, presystem, online_cpus);
                }
            }
            if let Some(mem) = st.memory_stats.as_ref() {
                // docker stats reports usage minus reclaimable page cache, not
                // raw cgroup usage: cgroup v2 exposes "inactive_file", v1
                // exposes "cache". Fall back to raw usage if neither is present.
                c.mem_used = mem.usage.map(|usage| {
                    let cache = mem.stats.as_ref().and_then(|m| {
                        m.get("inactive_file").or_else(|| m.get("cache")).copied()
                    });
                    let mem_used = cache.and_then(|c| usage.checked_sub(c)).unwrap_or(usage);
                    mem_used as f64
                });
                c.mem_limit = mem.limit.map(|l| l as f64);
                if let (Some(u), Some(l)) = (c.mem_used, c.mem_limit) {
                    if l > 0.0 {
                        c.mem_percent = Some(u / l * 100.0);
                    }
                }
            }
            if let Some(nets) = st.networks.as_ref() {
                c.net_rx = Some(nets.values().filter_map(|n| n.rx_bytes).sum::<u64>() as f64);
                c.net_tx = Some(nets.values().filter_map(|n| n.tx_bytes).sum::<u64>() as f64);
            }
            c.pids = st.pids_stats.as_ref().and_then(|p| p.current);
            if let Some(entries) = st
                .blkio_stats
                .as_ref()
                .and_then(|b| b.io_service_bytes_recursive.as_ref())
            {
                let mut read_sum = 0u64;
                let mut write_sum = 0u64;
                for e in entries {
                    let val = e.value.unwrap_or(0);
                    match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
                        Some("read") => read_sum += val,
                        Some("write") => write_sum += val,
                        _ => {}
                    }
                }
                c.block_read = Some(read_sum as f64);
                c.block_write = Some(write_sum as f64);
            }
        }

        if let Ok(img) = docker.inspect_image(&c.image).await {
            c.local_digest = img
                .repo_digests
                .as_ref()
                .and_then(|v| v.first())
                .and_then(|d| d.split_once('@').map(|(_, h)| h.to_string()));
        }

        out.push(c);
    }
    Some(out)
}

/// Refresh the cache. Tries the CLI, then bollard, then records unavailable.
/// Once containers are collected, checks each image against its registry
/// concurrently to populate `image_update_available`.
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
    }))
    .await;
    for (c, upd) in containers.iter_mut().zip(checks) {
        c.image_update_available = upd;
    }

    *CACHE.write().unwrap() = Some(Cache {
        status: "ok",
        reason: String::new(),
        containers,
    });
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

    const PS: &str = r#"{"ID":"a67c38371df9","Names":"coolify-sentinel","Image":"ghcr.io/coollabsio/sentinel:0.0.21","State":"running","Status":"Up 20 hours (healthy)","HealthStatus":"healthy","RunningFor":"20 hours ago","CreatedAt":"2026-07-08 03:38:33 -0400 EDT","Ports":""}
{"ID":"b12cafe00042","Names":"flappy-db","Image":"postgres:16","State":"exited","Status":"Exited (1) 3 minutes ago","HealthStatus":"","RunningFor":"5 minutes ago","CreatedAt":"2026-07-08 01:00:00 -0400 EDT","Ports":""}"#;
    const STATS: &str = r#"{"ID":"a67c38371df9","Name":"coolify-sentinel","CPUPerc":"0.50%","MemUsage":"23.43MiB / 23.41GiB","MemPerc":"0.10%","NetIO":"1.11MB / 51.8MB","BlockIO":"32MB / 401kB","PIDs":"11"}"#;
    const INSPECT: &str = r#"[{"Id":"a67c38371df9abc","RestartCount":2,"Config":{"Image":"ghcr.io/coollabsio/sentinel:0.0.21"},"State":{"Health":{"Status":"healthy"}}},{"Id":"b12cafe00042abc","RestartCount":7,"Config":{"Image":"postgres:16"},"State":{}}]"#;

    #[test]
    fn get_container_stats_unavailable_before_refresh() {
        // With nothing cached, status is "unavailable" and containers is [].
        let v = get_container_stats();
        assert_eq!(v["status"], "unavailable");
        assert!(v["containers"].as_array().unwrap().is_empty());
    }

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

    #[tokio::test]
    #[ignore = "live: requires a reachable docker socket on this host"]
    async fn live_bollard_probe() {
        let c = collect_via_bollard().await.expect("bollard should reach the docker socket");
        eprintln!("collected {} containers via bollard", c.len());
        for x in c.iter().take(5) {
            eprintln!("{} | {} | {} | cpu={:?} mem={:?} restarts={:?}",
                x.name, x.state, x.health, x.cpu_percent, x.mem_used, x.restart_count);
            eprintln!(
                "    pids={:?} block_read={:?} block_write={:?} ports={:?} created_at={:?} uptime={:?}",
                x.pids, x.block_read, x.block_write, x.ports, x.created_at, x.uptime
            );
        }
        assert!(!c.is_empty());
    }

    #[test]
    fn cpu_percent_formula() {
        // cpu_delta=100, system_delta=1000, 4 cpus -> 100/1000*4*100 = 40%
        assert_eq!(cpu_percent(200, 100, 5000, 4000, 4), Some(40.0));
        // zero system delta -> None (no divide-by-zero, no fake 0)
        assert_eq!(cpu_percent(200, 100, 4000, 4000, 4), None);
    }

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
}
