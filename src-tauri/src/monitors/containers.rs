//! Docker container monitor. See docs/superpowers/specs/2026-07-08-container-monitoring-design.md
//!
//! Model + byte-string parsers land first (this task); the CLI/bollard
//! backends, cache and `get_container_stats()` that consume them land in
//! later tasks on this branch. Until then `Container` and the parsers below
//! are unused from the crate's perspective, hence the `dead_code` allows.
use serde::Serialize;

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
#[allow(dead_code)] // used by the CLI/bollard backends added in later tasks
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
#[allow(dead_code)] // used by the CLI/bollard backends added in later tasks
fn parse_pair(s: &str) -> (Option<f64>, Option<f64>) {
    let mut it = s.split('/');
    let a = it.next().and_then(parse_bytes);
    let b = it.next().and_then(parse_bytes);
    (a, b)
}

/// Parse a "12.5%" percentage into f64.
#[allow(dead_code)] // used by the CLI/bollard backends added in later tasks
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
