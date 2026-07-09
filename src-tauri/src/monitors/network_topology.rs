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
//! macOS reads connections from `lsof -nP -i -F` (unprivileged; connections +
//! owning PID, no byte counts). Windows reads them from `GetExtendedTcpTable`
//! (owner-PID table via the IP Helper API). Both classify the remote and report
//! `bytes_sent: null` (those sources give no bytes); on Linux the eBPF probe can
//! still supply true per-PID totals.

use serde_json::{json, Value};
use std::sync::Arc;
use crate::database::Database;
use crate::monitors::cloud_ranges;

// --- macOS: lsof -F reader ----------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    pub fn collect() -> Vec<Value> {
        // -n numeric IPs, -P numeric ports, -i internet only, -F field output.
        // `f` MUST be requested — it is the per-socket delimiter.
        let out = match std::process::Command::new("lsof")
            .args(["-nP", "-w", "-i", "-F", "pcfnPtT"])
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => return Vec::new(),
        };
        super::parse_lsof(&String::from_utf8_lossy(&out))
    }
}

/// Parse a host:port token ("1.2.3.4:443" or "[2606:4700::1]:443") to (ip,port).
#[cfg(any(target_os = "macos", test))]
fn parse_hostport(s: &str) -> Option<(std::net::IpAddr, u16)> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        let (ip, port) = rest.split_once("]:")?;
        return Some((ip.parse().ok()?, port.parse().ok()?));
    }
    let (ip, port) = s.rsplit_once(':')?;
    Some((ip.parse().ok()?, port.parse().ok()?))
}

/// Parse `lsof -nP -i -F pcfnPtT` output into egress connection JSON. State
/// machine: `p` starts a process, `f` starts a socket, `P`/`n`/`T` decorate it.
/// Only established TCP (and UDP with a remote peer) to non-local hosts are kept.
#[cfg(any(target_os = "macos", test))]
fn parse_lsof(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pid: Option<u32> = None;
    let mut cmd: Option<String> = None;
    let mut proto = String::new();
    let mut name = String::new();
    let mut state = String::new();
    let mut in_socket = false;

    fn flush(
        out: &mut Vec<Value>,
        pid: Option<u32>,
        cmd: &Option<String>,
        proto: &str,
        name: &str,
        state: &str,
    ) {
        // Only connected sockets (local->remote); listeners/unconnected UDP have no "->".
        let remote = match name.split_once("->") {
            Some((_, r)) => r,
            None => return,
        };
        // TCP must be ESTABLISHED; UDP has no state line (state empty => keep).
        if proto == "TCP" && !state.is_empty() && state != "ESTABLISHED" {
            return;
        }
        if let Some((ip, port)) = parse_hostport(remote) {
            let st = if state.is_empty() { "ESTABLISHED" } else { state };
            if let Some(v) = conn_json(cmd.clone(), pid, ip, port, st) {
                out.push(v);
            }
        }
    }

    for line in text.lines() {
        let tag = match line.chars().next() {
            Some(c) => c,
            None => continue,
        };
        let val = &line[tag.len_utf8()..];
        match tag {
            'p' => {
                if in_socket {
                    flush(&mut out, pid, &cmd, &proto, &name, &state);
                    in_socket = false;
                }
                pid = val.parse().ok();
                cmd = None;
                proto.clear();
                name.clear();
                state.clear();
            }
            'c' => cmd = Some(val.to_string()),
            'f' => {
                if in_socket {
                    flush(&mut out, pid, &cmd, &proto, &name, &state);
                }
                proto.clear();
                name.clear();
                state.clear();
                in_socket = true;
            }
            'P' => proto = val.to_string(),
            'n' => name = val.to_string(),
            'T' => {
                if let Some(st) = val.strip_prefix("ST=") {
                    state = st.to_string();
                }
            }
            _ => {}
        }
    }
    if in_socket {
        flush(&mut out, pid, &cmd, &proto, &name, &state);
    }
    out
}

// --- Windows: GetExtendedTcpTable reader --------------------------------------

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::ptr::addr_of;
    use ::windows::core::PWSTR;
    use ::windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, NO_ERROR};
    use ::windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use ::windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    use ::windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const MIB_TCP_STATE_ESTAB: u32 = 5;

    /// Ports are network byte order in the low 16 bits of a DWORD.
    fn ntohs(dw: u32) -> u16 {
        u16::from_be_bytes([(dw & 0xff) as u8, ((dw >> 8) & 0xff) as u8])
    }

    /// Two-call idiom + 4-byte-aligned Vec<u32> backing buffer.
    unsafe fn fetch_aligned(
        mut call: impl FnMut(Option<*mut c_void>, *mut u32) -> u32,
    ) -> Option<Vec<u32>> {
        let mut size: u32 = 0;
        let r = call(None, &mut size);
        if r != ERROR_INSUFFICIENT_BUFFER.0 && r != NO_ERROR.0 {
            return None;
        }
        if size == 0 {
            return Some(Vec::new());
        }
        let words = (size as usize + 3) / 4;
        let mut buf = vec![0u32; words];
        let r = call(Some(buf.as_mut_ptr() as *mut c_void), &mut size);
        if r != NO_ERROR.0 {
            return None;
        }
        Some(buf)
    }

    /// (remote_ip, remote_port, owning_pid) for each ESTABLISHED TCP connection.
    unsafe fn established_tcp() -> Vec<(IpAddr, u16, u32)> {
        let mut out = Vec::new();
        // IPv4
        if let Some(buf) = fetch_aligned(|ptr, size| {
            GetExtendedTcpTable(ptr, size, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
        }) {
            if !buf.is_empty() {
                let table = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
                let rows = addr_of!((*table).table) as *const MIB_TCPROW_OWNER_PID;
                for i in 0..(*table).dwNumEntries as usize {
                    let row = &*rows.add(i);
                    if row.dwState != MIB_TCP_STATE_ESTAB {
                        continue;
                    }
                    let ip = IpAddr::V4(Ipv4Addr::from(row.dwRemoteAddr.to_le_bytes()));
                    out.push((ip, ntohs(row.dwRemotePort), row.dwOwningPid));
                }
            }
        }
        // IPv6
        if let Some(buf) = fetch_aligned(|ptr, size| {
            GetExtendedTcpTable(ptr, size, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)
        }) {
            if !buf.is_empty() {
                let table = buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID;
                let rows = addr_of!((*table).table) as *const MIB_TCP6ROW_OWNER_PID;
                for i in 0..(*table).dwNumEntries as usize {
                    let row = &*rows.add(i);
                    if row.dwState != MIB_TCP_STATE_ESTAB {
                        continue;
                    }
                    let ip = IpAddr::V6(Ipv6Addr::from(row.ucRemoteAddr));
                    out.push((ip, ntohs(row.dwRemotePort), row.dwOwningPid));
                }
            }
        }
        out
    }

    /// PID -> executable file name via Win32 (needs no elevation, no extra crate).
    fn pid_to_name(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let handle: HANDLE =
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; 32_768];
            let mut len = buf.len() as u32;
            let res = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            res.ok()?;
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            // Just the file name, matching the lsof/`comm` style used elsewhere.
            Some(
                path.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&path)
                    .to_string(),
            )
        }
    }

    pub fn collect() -> Vec<Value> {
        let conns = unsafe { established_tcp() };
        let mut names: HashMap<u32, Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for (remote_ip, remote_port, pid) in conns {
            let name = names.entry(pid).or_insert_with(|| pid_to_name(pid)).clone();
            if let Some(v) = conn_json(name, Some(pid), remote_ip, remote_port, "ESTABLISHED") {
                out.push(v);
            }
        }
        out
    }
}

/// One established outbound connection with real telemetry (Linux reader).
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "macos")]
    {
        macos::collect()
    }
    #[cfg(target_os = "windows")]
    {
        windows::collect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // Honest empty state: no fabricated connections on unsupported platforms.
        Vec::new()
    }
}

/// Build a connection JSON for the macOS/Windows readers, which supply the
/// endpoints + owning process but no byte counts. Shape matches the Linux
/// reader (minus real bytes). Classifies the remote and never fabricates bytes
/// or cost. Returns None for local/loopback remotes (not egress).
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn conn_json(
    process: Option<String>,
    pid: Option<u32>,
    remote_ip: std::net::IpAddr,
    remote_port: u16,
    state: &str,
) -> Option<Value> {
    if cloud_ranges::is_local(remote_ip) {
        return None;
    }
    let class = cloud_ranges::classify(remote_ip);
    let provider = class.provider;
    let cost = if provider.is_mesh() { Some(0.0) } else { None::<f64> };
    Some(json!({
        "process": process.unwrap_or_else(|| "unknown".to_string()),
        "pid": pid,
        "destination_ip": remote_ip.to_string(),
        "destination_name": class.detail,
        "destination_port": remote_port,
        "provider": provider.label(),
        "is_mesh": provider.is_mesh(),
        "state": state,
        "bytes_sent": Option::<u64>::None,
        "bytes_acked": Option::<u64>::None,
        "estimated_cost_usd": cost,
        "attribution_confidence": if cloud_ranges::is_fallback() { "fallback" } else { "live" },
        "byte_accounting": "unavailable",
        "shadow_alert": pid.is_none() && provider.egress_usd_per_gb().is_some(),
    }))
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
            // Join on the full 4-tuple (local|remote) so multiple sockets to the
            // same server (connection pools) each get their own byte counts.
            let tuple_key = format!("{}|{}", c.local_key, c.remote_key);
            if let Some(&(sent, acked)) = ss_bytes.get(&tuple_key) {
                c.bytes_sent = Some(sent);
                c.bytes_acked = Some(acked);
            }
            out.push(to_json(&c, fallback));
        }

        // If the eBPF probe is active, annotate each connection with the owning
        // process's TRUE cumulative egress bytes (across all its sockets, incl.
        // closed ones) — something the per-socket ss view cannot provide.
        if let Some(per_pid) = crate::monitors::ebpf_egress::snapshot() {
            for conn in &mut out {
                if let Some(pid) = conn.get("pid").and_then(|p| p.as_u64()) {
                    if let Some(&total) = per_pid.get(&(pid as u32)) {
                        conn["process_egress_bytes"] = json!(total);
                    }
                }
            }
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
        let output = match std::process::Command::new("ss").args(["-tinH"]).output() {
            Ok(o) if o.status.success() => o.stdout,
            _ => return HashMap::new(),
        };
        parse_ss_output(&String::from_utf8_lossy(&output))
    }

    /// Parse `ss -tinH` into a map keyed by the full "local|remote" 4-tuple ->
    /// (bytes_sent, bytes_acked). Keying by the whole tuple (not just the peer)
    /// keeps distinct sockets to the same server from overwriting each other.
    fn parse_ss_output(text: &str) -> HashMap<String, (u64, u64)> {
        let mut map = HashMap::new();
        // Default `ss` format: a header line (State Recv-Q Send-Q Local Peer ...)
        // followed by a whitespace-indented info line carrying "bytes_sent:N".
        let mut current_key: Option<String> = None;
        for line in text.lines() {
            if line.starts_with(char::is_whitespace) {
                // Continuation info line for the current connection.
                if let Some(key) = &current_key {
                    if let Some(s) = extract_kv(line, "bytes_sent:") {
                        map.insert(key.clone(), (s, extract_kv(line, "bytes_acked:").unwrap_or(0)));
                    }
                }
            } else {
                // Header line: State Recv-Q Send-Q Local Peer.
                let cols: Vec<&str> = line.split_whitespace().collect();
                current_key = match (cols.get(3), cols.get(4)) {
                    (Some(local), Some(peer)) => {
                        Some(format!("{}|{}", normalize_peer(local), normalize_peer(peer)))
                    }
                    _ => None,
                };
                // Some ss builds print info inline on the header line too.
                if let Some(key) = &current_key {
                    if let Some(s) = extract_kv(line, "bytes_sent:") {
                        map.insert(key.clone(), (s, extract_kv(line, "bytes_acked:").unwrap_or(0)));
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
        fn ss_bytes_keyed_by_full_tuple_no_pool_collision() {
            // Two pooled sockets to the SAME server from different local ports
            // must keep their own byte counts (regression: peer-only keying
            // collapsed them to the last value).
            let sample = "\
ESTAB 0 0 10.0.0.1:50001 140.82.112.4:443
\t bbr rtt:5 bytes_sent:100 bytes_acked:100 segs_out:3
ESTAB 0 0 10.0.0.1:50002 140.82.112.4:443
\t bbr rtt:5 bytes_sent:200 bytes_acked:200 segs_out:5";
            let map = parse_ss_output(sample);
            assert_eq!(map.len(), 2, "two distinct sockets, two keys");
            assert_eq!(map.get("10.0.0.1:50001|140.82.112.4:443"), Some(&(100, 100)));
            assert_eq!(map.get("10.0.0.1:50002|140.82.112.4:443"), Some(&(200, 200)));
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

#[cfg(test)]
mod macos_parser_tests {
    use super::*;

    #[test]
    fn parse_lsof_extracts_egress_and_skips_local() {
        let sample = "\
p1234
cSafari
f10
PTCP
tIPv4
n192.168.1.5:52345->140.82.112.3:443
TST=ESTABLISHED
f11
PTCP
tIPv4
n10.0.0.1:52346->127.0.0.1:8080
TST=ESTABLISHED
f12
PTCP
tIPv4
n10.0.0.1:52347->140.82.112.9:443
TST=SYN_SENT
p5678
ccurl
f3
PTCP
tIPv4
n10.0.0.2:60001->34.149.66.137:443
TST=ESTABLISHED
f4
PUDP
tIPv4
n10.0.0.2:5353->8.8.8.8:53
p999
cmDNSResponder
f8
PUDP
tIPv4
n*:5353";
        let conns = parse_lsof(sample);
        // Kept: Safari->140.82.112.3 (ESTAB), curl->34.149.66.137 (ESTAB),
        // curl->8.8.8.8 (UDP, no state). Skipped: ->127.0.0.1 (loopback),
        // ->140.82.112.9 (SYN_SENT, not established), *:5353 (no remote).
        assert_eq!(conns.len(), 3, "got {:?}", conns);
        let dests: Vec<&str> = conns.iter().map(|c| c["destination_ip"].as_str().unwrap()).collect();
        assert!(dests.contains(&"140.82.112.3"));
        assert!(dests.contains(&"34.149.66.137"));
        assert!(dests.contains(&"8.8.8.8"));
        assert!(!dests.contains(&"127.0.0.1"));
        // bytes are honestly null (lsof gives no counters).
        assert!(conns.iter().all(|c| c["bytes_sent"].is_null()));
        // process name comes from the `c` line (full, untruncated).
        let safari = conns.iter().find(|c| c["destination_ip"] == "140.82.112.3").unwrap();
        assert_eq!(safari["process"], "Safari");
    }
}
