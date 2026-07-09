//! Egress topology: which local processes are talking to which remote networks,
//! how many bytes they have sent, and a rough cost attribution per cloud.
//!
//! This replaces the previous fully-mocked implementation. On Linux it reads
//! real data:
//!   * `/proc/net/tcp` + `/proc/net/tcp6` -> established connections, remote IP,
//!     socket inode (stable kernel format).
//!   * `/proc/<pid>/fd` -> maps socket inode to the owning PID and process name.
//!   * `ss -tinH` -> real cumulative `bytes_sent`/`bytes_acked` per socket from
//!     the kernel's `tcp_info` (via the sock_diag netlink API).
//!   * `cloud_ranges::classify` -> provider attribution + egress cost.
//!
//! It returns ONLY real connections; when nothing qualifies (or `/proc` can't be
//! read) it returns an empty list — never fabricated data. Per-PID byte
//! accounting across short-lived / already-closed sockets is out of reach for
//! `/proc`+`ss` and is the job of the eBPF probe on the roadmap (see README).
//!
//! macOS and Windows are not implemented yet and return an empty list rather
//! than a mock (see roadmap).

use serde_json::{json, Value};
use std::sync::Arc;
use crate::database::Database;
use crate::monitors::cloud_ranges;

/// One established outbound connection with real telemetry.
struct Conn {
    pid: Option<u32>,
    process: Option<String>,
    remote_ip: std::net::IpAddr,
    remote_port: u16,
    local_key: String,
    remote_key: String,
    bytes_sent: Option<u64>,
    bytes_acked: Option<u64>,
}

pub fn get_egress_topology(_db: &Arc<Database>) -> Vec<Value> {
    #[cfg(target_os = "linux")]
    {
        linux::collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Honest empty state: no fabricated connections on unsupported platforms.
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub fn collect() -> Vec<Value> {
        let mut conns = Vec::new();
        // Parse both address families; established, non-local remotes only.
        for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(text) = std::fs::read_to_string(path) {
                parse_proc_net_tcp(&text, &mut conns);
            }
        }
        if conns.is_empty() {
            return Vec::new();
        }

        // Enrich with owning process and real byte counts.
        let inode_to_pid = build_inode_pid_map();
        let ss_bytes = collect_ss_bytes();

        let fallback = cloud_ranges::is_fallback();
        let mut out = Vec::new();
        for (mut c, inode) in conns {
            if let Some(&pid) = inode_to_pid.get(&inode) {
                c.pid = Some(pid);
                c.process = read_comm(pid);
            }
            if let Some(&(sent, acked)) = ss_bytes.get(&c.remote_key).or_else(|| ss_bytes.get(&c.local_key)) {
                c.bytes_sent = Some(sent);
                c.bytes_acked = Some(acked);
            }
            out.push(to_json(&c, fallback));
        }
        out
    }

    /// Parse the kernel's `/proc/net/tcp{,6}` table. Pushes established
    /// connections whose remote endpoint is not local, paired with the inode.
    fn parse_proc_net_tcp(text: &str, out: &mut Vec<(Conn, u64)>) {
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // Need through the inode field (index 9).
            if f.len() < 10 {
                continue;
            }
            // st field (index 3): 01 = ESTABLISHED.
            if f[3] != "01" {
                continue;
            }
            let (local_ip, local_port) = match parse_hex_addr(f[1]) {
                Some(v) => v,
                None => continue,
            };
            let (remote_ip, remote_port) = match parse_hex_addr(f[2]) {
                Some(v) => v,
                None => continue,
            };
            // Only outbound-to-non-local endpoints count as egress. CGNAT/mesh
            // is classified (not local) so it is kept.
            if cloud_ranges::is_local(remote_ip) {
                continue;
            }
            let inode: u64 = match f[9].parse() {
                Ok(i) => i,
                Err(_) => continue,
            };
            out.push((
                Conn {
                    pid: None,
                    process: None,
                    remote_ip,
                    remote_port,
                    local_key: format!("{local_ip}:{local_port}"),
                    remote_key: format!("{remote_ip}:{remote_port}"),
                    bytes_sent: None,
                    bytes_acked: None,
                },
                inode,
            ));
        }
    }

    /// Parse a `/proc/net/tcp` hex `ADDR:PORT` field into (ip, port).
    /// IPv4 is a single little-endian 32-bit word; IPv6 is four LE 32-bit words.
    fn parse_hex_addr(field: &str) -> Option<(IpAddr, u16)> {
        let (addr_hex, port_hex) = field.split_once(':')?;
        let port = u16::from_str_radix(port_hex, 16).ok()?;
        match addr_hex.len() {
            8 => {
                let v = u32::from_str_radix(addr_hex, 16).ok()?;
                let ip = Ipv4Addr::from(v.to_le_bytes());
                Some((IpAddr::V4(ip), port))
            }
            32 => {
                let mut bytes = [0u8; 16];
                for word in 0..4 {
                    let chunk = &addr_hex[word * 8..word * 8 + 8];
                    let v = u32::from_str_radix(chunk, 16).ok()?;
                    bytes[word * 4..word * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                Some((IpAddr::V6(Ipv6Addr::from(bytes)), port))
            }
            _ => None,
        }
    }

    /// Scan `/proc/<pid>/fd/*` symlinks for `socket:[inode]` -> pid.
    /// Best-effort: covers the current user's processes always, others only with
    /// sufficient privilege. Missing coverage yields "unknown" process, never a
    /// guess.
    fn build_inode_pid_map() -> HashMap<u64, u32> {
        let mut map = HashMap::new();
        let proc = match std::fs::read_dir("/proc") {
            Ok(p) => p,
            Err(_) => return map,
        };
        for entry in proc.flatten() {
            let name = entry.file_name();
            let pid: u32 = match name.to_str().and_then(|s| s.parse().ok()) {
                Some(p) => p,
                None => continue, // not a pid dir
            };
            let fd_dir = entry.path().join("fd");
            let fds = match std::fs::read_dir(&fd_dir) {
                Ok(f) => f,
                Err(_) => continue, // permission denied for another user's proc
            };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    if let Some(inode) = target
                        .to_str()
                        .and_then(|s| s.strip_prefix("socket:["))
                        .and_then(|s| s.strip_suffix(']'))
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        map.entry(inode).or_insert(pid);
                    }
                }
            }
        }
        map
    }

    fn read_comm(pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Run `ss -tinH` and map "peer_ip:port" -> (bytes_sent, bytes_acked) from
    /// the kernel tcp_info. Returns empty if `ss` is unavailable/unparseable, in
    /// which case byte counts stay `null` rather than being invented.
    fn collect_ss_bytes() -> HashMap<String, (u64, u64)> {
        let mut map = HashMap::new();
        let output = match std::process::Command::new("ss").args(["-tinH"]).output() {
            Ok(o) if o.status.success() => o.stdout,
            _ => return map,
        };
        let text = String::from_utf8_lossy(&output);
        // Default `ss` format: a header line (State Recv-Q Send-Q Local Peer ...)
        // followed by a whitespace-indented info line carrying "bytes_sent:N".
        let mut current_peer: Option<String> = None;
        for line in text.lines() {
            if line.starts_with(char::is_whitespace) {
                // Continuation info line for the current connection.
                if let Some(peer) = &current_peer {
                    let sent = extract_kv(line, "bytes_sent:");
                    let acked = extract_kv(line, "bytes_acked:");
                    if let Some(s) = sent {
                        map.insert(peer.clone(), (s, acked.unwrap_or(0)));
                    }
                }
            } else {
                // Header line: columns State Recv-Q Send-Q Local Peer.
                let cols: Vec<&str> = line.split_whitespace().collect();
                current_peer = cols.get(4).map(|s| normalize_peer(s));
                // Some ss builds print info inline on the header line too.
                if let Some(peer) = &current_peer {
                    if let Some(s) = extract_kv(line, "bytes_sent:") {
                        let acked = extract_kv(line, "bytes_acked:").unwrap_or(0);
                        map.insert(peer.clone(), (s, acked));
                    }
                }
            }
        }
        map
    }

    /// ss prints IPv6 peers as `[addr]:port`; our keys use `addr:port`.
    fn normalize_peer(s: &str) -> String {
        if let Some(rest) = s.strip_prefix('[') {
            if let Some((addr, port)) = rest.split_once("]:") {
                return format!("{addr}:{port}");
            }
        }
        s.to_string()
    }

    fn extract_kv(line: &str, key: &str) -> Option<u64> {
        let idx = line.find(key)?;
        let after = &line[idx + key.len()..];
        let num: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        num.parse().ok()
    }

    fn to_json(c: &Conn, fallback_ranges: bool) -> Value {
        let class = cloud_ranges::classify(c.remote_ip);
        let provider = class.provider;
        let cost = match (c.bytes_sent, provider.egress_usd_per_gb()) {
            (Some(bytes), Some(rate)) => Some(bytes as f64 / 1.0e9 * rate),
            (_, None) if provider.is_mesh() => Some(0.0), // mesh egress is free
            _ => None, // unknown bytes or unattributed provider -> no fake number
        };
        let process = c.process.clone().unwrap_or_else(|| "unknown".to_string());
        // A "shadow" connection: reaches a metered public cloud but we could not
        // attribute an owning process (e.g. another user's socket, no privilege).
        let shadow_alert = c.pid.is_none() && provider.egress_usd_per_gb().is_some();

        json!({
            "process": process,
            "pid": c.pid,
            "destination_ip": c.remote_ip.to_string(),
            "destination_name": class.detail,
            "destination_port": c.remote_port,
            "provider": provider.label(),
            "is_mesh": provider.is_mesh(),
            "bytes_sent": c.bytes_sent,
            "bytes_acked": c.bytes_acked,
            "estimated_cost_usd": cost,
            "attribution_confidence": if fallback_ranges { "fallback" } else { "live" },
            "byte_accounting": if c.bytes_sent.is_some() { "tcp_info" } else { "unavailable" },
            "shadow_alert": shadow_alert,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_ipv4_hex_addr_little_endian() {
            // 0100007F:0035 -> 127.0.0.1:53
            let (ip, port) = parse_hex_addr("0100007F:0035").unwrap();
            assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap());
            assert_eq!(port, 53);
            // 0470528C:01BB -> 140.82.112.4:443 (GitHub)
            let (ip, port) = parse_hex_addr("0470528C:01BB").unwrap();
            assert_eq!(ip, "140.82.112.4".parse::<IpAddr>().unwrap());
            assert_eq!(port, 443);
        }

        #[test]
        fn parses_ipv6_hex_addr() {
            // ::1 loopback: last word = 0x01000000 (LE), rest 0.
            let (ip, port) = parse_hex_addr("00000000000000000000000001000000:0050").unwrap();
            assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
            assert_eq!(port, 80);
        }

        #[test]
        fn skips_non_established_and_local_remotes() {
            // st=0A (LISTEN) and a local remote must both be skipped.
            let sample = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 111 1 0 0
   1: 0100007F:8000 0100007F:1F90 01 00000000:00000000 00:00000000 00000000 0 0 222 1 0 0
   2: 0470528C:8000 0470528C:01BB 01 00000000:00000000 00:00000000 00000000 0 0 333 1 0 0";
            // Rewrite column 2 of line 2 to a public remote to prove it is kept.
            let mut v = Vec::new();
            parse_proc_net_tcp(sample, &mut v);
            // Line 0: LISTEN -> skipped. Line 1: remote 127.0.0.1 -> local, skipped.
            // Line 2: remote 140.82.112.4 -> kept.
            assert_eq!(v.len(), 1, "only the one public established conn is kept");
            assert_eq!(v[0].1, 333); // inode
            assert_eq!(v[0].0.remote_ip, "140.82.112.4".parse::<IpAddr>().unwrap());
        }

        #[test]
        fn extract_kv_reads_ss_bytes() {
            let line = "\t bbr rtt:5 bytes_sent:865 bytes_acked:866 segs_out:35";
            assert_eq!(extract_kv(line, "bytes_sent:"), Some(865));
            assert_eq!(extract_kv(line, "bytes_acked:"), Some(866));
            assert_eq!(extract_kv(line, "bytes_received:"), None);
        }

        #[test]
        fn normalize_peer_strips_ipv6_brackets() {
            assert_eq!(normalize_peer("[2606:4700::1]:443"), "2606:4700::1:443");
            assert_eq!(normalize_peer("140.82.112.4:443"), "140.82.112.4:443");
        }

        #[test]
        #[ignore = "runtime probe: exercises real /proc/net + ss on this host"]
        fn live_probe_prints_real_connections() {
            let conns = collect();
            eprintln!("=== live egress connections: {} ===", conns.len());
            for c in conns.iter().take(12) {
                eprintln!("{}", serde_json::to_string(c).unwrap());
            }
            // Structural invariants over whatever real data exists on this host:
            for c in &conns {
                assert!(c["destination_ip"].is_string());
                assert!(c["provider"].is_string());
                // bytes_sent is a real count or null — never fabricated.
                assert!(c["bytes_sent"].is_u64() || c["bytes_sent"].is_null());
                // cost is a real number or null — never invented for unknowns.
                assert!(c["estimated_cost_usd"].is_number() || c["estimated_cost_usd"].is_null());
            }
        }
    }
}
