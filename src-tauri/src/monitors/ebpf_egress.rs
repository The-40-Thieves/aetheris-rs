//! Userspace side of the eBPF egress byte-accounting probe.
//!
//! When built with `--features ebpf` and run with sufficient privilege
//! (root / CAP_BPF + CAP_NET_ADMIN), [`try_load`] loads the embedded probe and
//! attaches the `tcp_sendmsg`/`udp_sendmsg` kprobes. [`snapshot`] then reads the
//! kernel map of `pid -> cumulative egress payload bytes`, giving true per-PID
//! accounting that includes short-lived/closed sockets and daemons with no
//! currently-tracked connection — the thing the `/proc`+`ss` reader cannot see.
//!
//! Without the feature (or without privilege to load), everything degrades to a
//! no-op and the egress reader keeps its connection-level view. Nothing here
//! ever fabricates data.

/// Whether the eBPF byte-accounting probe is loaded and active.
pub fn is_active() -> bool {
    imp::is_active()
}

/// Attempt to load + attach the probe. Returns whether it became active.
/// Safe to call once at startup; failure (no feature / no privilege) is a no-op.
pub fn try_load() -> bool {
    imp::try_load()
}

/// Current `pid -> cumulative egress bytes` snapshot, if the probe is active.
pub fn snapshot() -> Option<std::collections::HashMap<u32, u64>> {
    imp::snapshot()
}

/// Top-N processes by cumulative egress bytes, as frontend-ready JSON
/// (`{pid, process, bytesSent}`), resolving the process name from `/proc`.
/// Empty when the probe is inactive.
pub fn per_process_rollup(top_n: usize) -> Vec<serde_json::Value> {
    let map = match snapshot() {
        Some(m) => m,
        None => return Vec::new(),
    };
    let mut rows: Vec<(u32, u64)> = map.into_iter().collect();
    rows.sort_by_key(|&(_, bytes)| std::cmp::Reverse(bytes));
    rows.truncate(top_n);
    rows.into_iter()
        .map(|(pid, bytes)| {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            serde_json::json!({ "pid": pid, "process": comm, "bytesSent": bytes })
        })
        .collect()
}

#[cfg(feature = "ebpf")]
mod imp {
    use aya::{
        maps::HashMap as AyaHashMap,
        programs::{KProbe, TracePoint},
        Ebpf,
    };
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    // Holds the loaded probe for the process lifetime; the attached kprobes stay
    // live as long as the Ebpf is held. None once we've tried and failed.
    static PROBE: OnceLock<Mutex<Option<Ebpf>>> = OnceLock::new();

    fn cell() -> &'static Mutex<Option<Ebpf>> {
        PROBE.get_or_init(|| Mutex::new(None))
    }

    pub fn is_active() -> bool {
        cell().lock().unwrap().is_some()
    }

    pub fn try_load() -> bool {
        let mut guard = cell().lock().unwrap();
        if guard.is_some() {
            return true;
        }
        match load_and_attach() {
            Ok(bpf) => {
                *guard = Some(bpf);
                println!("[ebpf] egress probe loaded (tcp_sendmsg/udp_sendmsg kprobes attached)");
                true
            }
            Err(e) => {
                eprintln!("[ebpf] egress probe not loaded (needs root/CAP_BPF): {e}");
                false
            }
        }
    }

    type BoxErr = Box<dyn std::error::Error + Send + Sync>;

    fn load_and_attach() -> Result<Ebpf, BoxErr> {
        // Object embedded from OUT_DIR by build.rs.
        let obj = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/aetheris-ebpf"));
        let mut bpf = Ebpf::load(obj)?;
        for name in ["tcp_sendmsg", "udp_sendmsg"] {
            let prog: &mut KProbe = bpf
                .program_mut(name)
                .ok_or_else(|| format!("program {name} missing from object"))?
                .try_into()?;
            prog.load()?;
            prog.attach(name, 0)?;
        }
        // Evict per-process totals on process exit (prevents PID-reuse
        // misattribution and unbounded map growth).
        {
            let prog: &mut TracePoint = bpf
                .program_mut("sched_process_exit")
                .ok_or("program sched_process_exit missing from object")?
                .try_into()?;
            prog.load()?;
            prog.attach("sched", "sched_process_exit")?;
        }
        Ok(bpf)
    }

    pub fn snapshot() -> Option<HashMap<u32, u64>> {
        let guard = cell().lock().unwrap();
        let bpf = guard.as_ref()?;
        let map: AyaHashMap<_, u32, u64> = AyaHashMap::try_from(bpf.map("EGRESS_BYTES")?).ok()?;
        Some(map.iter().filter_map(|r| r.ok()).collect())
    }
}

#[cfg(not(feature = "ebpf"))]
mod imp {
    use std::collections::HashMap;

    pub fn is_active() -> bool {
        false
    }
    pub fn try_load() -> bool {
        false
    }
    pub fn snapshot() -> Option<HashMap<u32, u64>> {
        None
    }
}

#[cfg(all(test, feature = "ebpf"))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "live: loads the embedded eBPF probe; requires root / CAP_BPF"]
    fn live_probe_loads_and_reads_per_process_egress() {
        assert!(try_load(), "probe should load and attach under sufficient privilege");
        assert!(is_active());
        // Generate a little egress so the map is non-empty.
        let _ = std::process::Command::new("curl")
            .args(["-s", "-o", "/dev/null", "https://crates.io"])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(700));
        let rollup = per_process_rollup(8);
        eprintln!("=== eBPF egress by process ({} rows) ===", rollup.len());
        for r in &rollup {
            eprintln!("  {r}");
        }
        assert!(!rollup.is_empty(), "should observe real per-PID egress bytes");
        // Honesty: every reported value is a real u64 from the kernel map.
        for r in &rollup {
            assert!(r["bytesSent"].as_u64().is_some());
        }
    }
}
