//! Docker container monitor. See docs/superpowers/specs/2026-07-08-container-monitoring-design.md
//!
//! Model + byte-string parsers land first (this task); the CLI/bollard
//! backends, cache and `get_container_stats()` that consume them land in
//! later tasks on this branch. Until then `Container` and the parsers below
//! are unused from the crate's perspective, hence the `dead_code` allows.
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Serialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // constructed by the collection backends added in later tasks
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
#[allow(dead_code)] // consumed by merge_cli's callers, wired up in Task 3
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
#[allow(dead_code)] // consumed by merge_cli's callers, wired up in Task 3
fn parse_pair(s: &str) -> (Option<f64>, Option<f64>) {
    let mut it = s.split('/');
    let a = it.next().and_then(parse_bytes);
    let b = it.next().and_then(parse_bytes);
    (a, b)
}

/// Parse a "12.5%" percentage into f64.
#[allow(dead_code)] // consumed by merge_cli's callers, wired up in Task 3
fn parse_percent(s: &str) -> Option<f64> {
    s.trim().trim_end_matches('%').trim().parse().ok()
}

#[allow(dead_code)] // consumed by merge_cli's callers, wired up in Task 3
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

#[allow(dead_code)] // consumed by merge_cli's callers, wired up in Task 3
fn health_from(s: &str) -> String {
    match s {
        "healthy" | "unhealthy" | "starting" => s.to_string(),
        _ => "none".to_string(),
    }
}

/// Join `docker ps`/`stats` (newline-delimited JSON) and `docker inspect` (JSON
/// array) by 12-char container id into normalized Containers.
#[allow(dead_code)] // consumed by collect_via_cli(), wired up in Task 3
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
