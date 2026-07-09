#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde_json::{json, Value};
use sysinfo::{System, Networks, Disks, Components};

mod database;
mod monitors;
mod analytics;
mod server;

use std::sync::Arc;

/// A cached value with the instant it was produced.
struct Cached {
    at: Instant,
    val: Value,
}

/// Per-category caches for the slow-changing / subprocess-spawning parts of
/// get_stats, so a 1 Hz poll doesn't run smartctl / nvidia-smi / a /proc scan
/// every second. Fast metrics (CPU/mem/net/processes/sensors) are never cached.
#[derive(Default)]
struct Caches {
    inventory: Mutex<Option<Cached>>, // ais + toolchains + vpns + apps (5 min)
    gpus: Mutex<Option<Cached>>,      // 30 s
    smart_disks: Mutex<Option<Cached>>, // 60 s
    batteries: Mutex<Option<Cached>>, // 30 s
    egress: Mutex<Option<Cached>>,    // 5 s (expensive /proc/*/fd scan)
    baselines: Mutex<Option<Cached>>, // 60 s
}

/// Return the cached value if still fresh, else recompute via `f`, store it, and
/// return it. Cloning a cached serde_json::Value is far cheaper than respawning
/// the subprocess / rescanning that produced it.
fn cached(slot: &Mutex<Option<Cached>>, ttl: Duration, f: impl FnOnce() -> Value) -> Value {
    let mut guard = slot.lock().unwrap();
    if let Some(c) = guard.as_ref() {
        if c.at.elapsed() < ttl {
            return c.val.clone();
        }
    }
    let val = f();
    *guard = Some(Cached { at: Instant::now(), val: val.clone() });
    val
}

const TTL_INVENTORY: Duration = Duration::from_secs(300);
const TTL_GPU: Duration = Duration::from_secs(30);
const TTL_SMART: Duration = Duration::from_secs(60);
const TTL_BATTERY: Duration = Duration::from_secs(30);
const TTL_EGRESS: Duration = Duration::from_secs(5);
const TTL_BASELINES: Duration = Duration::from_secs(60);

struct AppState {
    sys: Mutex<System>,
    networks: Mutex<Networks>,
    disks: Mutex<Disks>,
    components: Mutex<Components>,
    db: Arc<database::Database>,
    caches: Caches,
}

fn detect_ais(sys: &System) -> Vec<serde_json::Value> {
    let ai_tools = vec![
        ("Ollama", "ollama", "Local AI"),
        ("PLUR", "plur", "Local AI"),
        ("Antigravity", "agy", "Agentic IDE"),
        ("ChatGPT", "chatgpt", "Cloud AI"),
        ("Claude", "claude", "Cloud AI"),
        ("LM Studio", "lmstudio", "Local AI")
    ];
    let mut detected = Vec::new();
    for (name, bin, typ) in &ai_tools {
        let is_installed = which::which(bin).is_ok();
        let is_running = sys.processes().values().any(|p| p.name().to_string_lossy().to_lowercase().contains(bin));
        if is_installed || is_running {
            detected.push(json!({ "name": name, "type": typ, "installed": is_installed, "running": is_running }));
        }
    }
    detected
}

fn detect_toolchains() -> Vec<serde_json::Value> {
    let tools = vec![
        ("Node.js", "node"), ("Rust", "rustc"), ("Go", "go"),
        ("Python", "python"), ("Python3", "python3"), ("Docker", "docker"), ("Git", "git"),
    ];
    let mut detected = Vec::new();
    for (name, bin) in tools {
        if let Ok(path) = which::which(bin) {
            detected.push(json!({ "name": name, "path": path.to_string_lossy() }));
        }
    }
    detected
}

fn scan_vpn_mesh(sys: &System) -> Vec<serde_json::Value> {
    let vpns = vec![
        ("Tailscale", "tailscaled", "tailscale"),
        ("ZeroTier", "zerotier-one", "zerotier-cli"),
        ("WireGuard", "wg", "wg"),
        ("OpenVPN", "openvpn", "openvpn")
    ];
    let mut detected = Vec::new();
    for (name, proc_name, bin) in vpns {
        let is_installed = which::which(bin).is_ok();
        let is_running = sys.processes().values().any(|p| p.name().to_string_lossy().to_lowercase().contains(proc_name));
        if is_installed || is_running {
            detected.push(json!({ "name": name, "installed": is_installed, "running": is_running }));
        }
    }
    detected
}

fn scan_applications() -> Vec<String> {
    let mut apps = Vec::new();
    
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/usr/share/applications") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name.ends_with(".desktop") {
                        apps.push(name.replace(".desktop", ""));
                    }
                }
            }
        }
    }
    
    #[cfg(target_os = "macos")]
    {
        if let Ok(entries) = std::fs::read_dir("/Applications") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name.ends_with(".app") {
                        apps.push(name.replace(".app", ""));
                    }
                }
            }
        }
    }
    
    #[cfg(target_os = "windows")]
    {
        // Simple fallback for Windows without winreg, check Program Files
        if let Ok(entries) = std::fs::read_dir("C:\\Program Files") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    apps.push(name);
                }
            }
        }
    }
    
    apps.sort();
    apps.into_iter().take(20).collect()
}

#[tauri::command]
fn get_stats(state: tauri::State<AppState>) -> serde_json::Value {
    let mut sys = state.sys.lock().unwrap();
    let mut nets = state.networks.lock().unwrap();
    let mut disks = state.disks.lock().unwrap();
    let mut comps = state.components.lock().unwrap();
    
    // Refresh relevant info
    sys.refresh_all();
    nets.refresh(true);
    disks.refresh(true);
    comps.refresh(true);
    
    // Static info
    let host_name = sysinfo::System::host_name().unwrap_or_else(|| "Unknown".to_string());
    let os_name = sysinfo::System::name().unwrap_or_else(|| "Unknown".to_string());
    let os_version = sysinfo::System::os_version().unwrap_or_else(|| "Unknown".to_string());
    let kernel_version = sysinfo::System::kernel_version().unwrap_or_else(|| "Unknown".to_string());
    let cpu_arch = std::env::consts::ARCH.to_string();
    
    let cpus = sys.cpus();
    let cpu_brand = if !cpus.is_empty() { cpus[0].brand().to_string() } else { "Unknown".to_string() };
    // Get Static Specs
    let physical_cores = sysinfo::System::physical_core_count().unwrap_or(0);
    let logical_cores = sys.cpus().len();
    
    // Memory
    let total_mem = sys.total_memory();
    let used_mem = sys.used_memory();
    let free_mem = sys.free_memory();
    let available_mem = sys.available_memory();
    let mem_percent = if total_mem > 0 { (used_mem as f64 / total_mem as f64) * 100.0 } else { 0.0 };
    
    let total_swap = sys.total_swap();
    let used_swap = sys.used_swap();
    let swap_percent = if total_swap > 0 { (used_swap as f64 / total_swap as f64) * 100.0 } else { 0.0 };
    
    // Uptime
    let uptime = sysinfo::System::uptime();
    
    // Global CPU load
    let current_load = sys.global_cpu_usage();
    let cpus_load: Vec<f32> = cpus.iter().map(|c| c.cpu_usage()).collect();
    
    // Processes (Top 8 CPU, Top 8 Mem)
    let mut proc_list: Vec<_> = sys.processes().values().collect();
    let total_procs = proc_list.len();
    
    proc_list.sort_by(|a, b| b.cpu_usage().partial_cmp(&a.cpu_usage()).unwrap_or(std::cmp::Ordering::Equal));
    let list_cpu: Vec<_> = proc_list.iter().take(8).map(|p| {
        json!({
            "pid": p.pid().as_u32(),
            "name": p.name().to_string_lossy(),
            "cpu": (p.cpu_usage() * 10.0).round() / 10.0,
            "mem": (p.memory() as f64 / total_mem as f64 * 100.0).round() / 10.0,
            "disk_r": p.disk_usage().read_bytes,
            "disk_w": p.disk_usage().written_bytes,
            "state": format!("{:?}", p.status()),
            "user": p.user_id().map(|u| u.to_string()).unwrap_or_default()
        })
    }).collect();
    
    proc_list.sort_by_key(|p| std::cmp::Reverse(p.memory()));
    let list_mem: Vec<_> = proc_list.iter().take(8).map(|p| {
        json!({
            "pid": p.pid().as_u32(),
            "name": p.name().to_string_lossy(),
            "cpu": (p.cpu_usage() * 10.0).round() / 10.0,
            "mem": (p.memory() as f64 / total_mem as f64 * 100.0).round() / 10.0,
            "disk_r": p.disk_usage().read_bytes,
            "disk_w": p.disk_usage().written_bytes,
            "state": format!("{:?}", p.status()),
            "user": p.user_id().map(|u| u.to_string()).unwrap_or_default()
        })
    }).collect();
    
    let running_procs = sys.processes().values().filter(|p| format!("{:?}", p.status()) == "Run").count();

    // Networks
    let network_stats: Vec<_> = nets.iter().map(|(name, data)| {
        json!({
            "iface": name,
            "operstate": "up", // Sysinfo doesn't easily expose this, defaulting to up for active interfaces
            "rx_sec": data.received(),
            "tx_sec": data.transmitted(),
            "ip4": "",
            "ip6": "",
            "mac": data.mac_address().to_string()
        })
    }).collect();

    // Disks
    let disk_stats: Vec<_> = disks.list().iter().map(|d| {
        json!({
            "mount": d.mount_point().to_string_lossy(),
            "size": d.total_space(),
            "used": d.total_space() - d.available_space(),
            "available": d.available_space(),
            "usePercent": if d.total_space() > 0 { ((d.total_space() - d.available_space()) as f64 / d.total_space() as f64 * 100.0).round() / 10.0 } else { 0.0 }
        })
    }).collect();

    // Sensors
    let sensor_stats: Vec<_> = comps.iter().map(|c| {
        json!({
            "label": c.label(),
            "temp": c.temperature(),
            "max": c.max(),
            "critical": c.critical()
        })
    }).collect();

    // Cache the slow-changing / subprocess-spawning extras by change-rate so a
    // 1 Hz poll doesn't respawn smartctl/nvidia-smi or rescan /proc every second.
    let inventory = cached(&state.caches.inventory, TTL_INVENTORY, || json!({
        "ais": detect_ais(&sys),
        "toolchains": detect_toolchains(),
        "vpns": scan_vpn_mesh(&sys),
        "apps": scan_applications(),
    }));
    let gpus = cached(&state.caches.gpus, TTL_GPU, || json!(monitors::gpu::get_gpu_stats()));
    let smart_disks = cached(&state.caches.smart_disks, TTL_SMART, || {
        json!(monitors::smart_disk::get_smart_data().into_iter().map(|mut disk| {
            let model = disk["model"].as_str().unwrap_or("Unknown").to_string();
            let bw = disk["bytesWritten"].as_f64().unwrap_or(0.0);
            disk["rul"] = analytics::rul::calculate_ssd_rul(&state.db, &model, bw);
            disk
        }).collect::<Vec<_>>())
    });
    let batteries = cached(&state.caches.batteries, TTL_BATTERY, || {
        json!(monitors::battery::get_battery_stats().into_iter().map(|mut batt| {
            let soh = batt["stateOfHealth"].as_f64().unwrap_or(100.0);
            let cc = batt["cycleCount"].as_i64().unwrap_or(0) as i32;
            let ctx = format!(
                "{} {}",
                batt["vendor"].as_str().unwrap_or("Unknown"),
                batt["model"].as_str().unwrap_or("Unknown")
            );
            batt["rul"] = analytics::rul::calculate_battery_rul(&state.db, &ctx, soh, cc);
            batt
        }).collect::<Vec<_>>())
    });
    let egress = cached(&state.caches.egress, TTL_EGRESS, || {
        json!({
            "topology": monitors::network_topology::get_egress_topology(&state.db),
            // True per-PID cumulative egress bytes from the eBPF probe (incl.
            // closed/short-lived sockets and daemons with no tracked connection).
            // Empty when the probe isn't loaded; the frontend falls back gracefully.
            "byProcess": monitors::ebpf_egress::per_process_rollup(12),
            "accounting": if monitors::ebpf_egress::is_active() { "ebpf" } else { "sockets" },
        })
    });
    let baselines = cached(&state.caches.baselines, TTL_BASELINES, || {
        monitors::external_baselines::get_external_baselines(&state.db)
    });

    json!({
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),
        "static": {
            "os": {
                "hostname": host_name,
                "distro": os_name,
                "release": os_version,
                "kernel": kernel_version,
                "arch": cpu_arch
            },
            "cpu": {
                "brand": cpu_brand,
                "physicalCores": physical_cores,
                "cores": logical_cores,
                "speed": if !cpus.is_empty() { cpus[0].frequency() as f64 / 1000.0 } else { 0.0 }
            },
            "mem": {
                "total": total_mem
            },
            "system": {
                "manufacturer": "N/A", // Handled by separate crate or tool if needed
                "model": "N/A",
                "virtual": false
            },
            "bios": {
                "vendor": "N/A",
                "version": "N/A"
            },
            "baseboard": {
                "model": "N/A"
            }
        },
        "dynamic": {
            "uptime": uptime,
            "mem": {
                "used": used_mem,
                "free": free_mem,
                "active": used_mem,
                "available": available_mem,
                "percentUsed": (mem_percent * 10.0).round() / 10.0,
                "swaptotal": total_swap,
                "swapused": used_swap,
                "swapPercentUsed": (swap_percent * 10.0).round() / 10.0
            },
            "cpu": {
                "currentLoad": (current_load * 10.0).round() / 10.0,
                "currentLoadUser": 0.0,
                "currentLoadSystem": 0.0,
                "currentLoadCpu": cpus_load
            },
            "disk": disk_stats,
            "network": network_stats,
            "processes": {
                "all": total_procs,
                "running": running_procs,
                "listCpu": list_cpu,
                "listMem": list_mem
            },
            "extras": {
                "sensors": sensor_stats,
                "ais": inventory["ais"].clone(),
                "toolchains": inventory["toolchains"].clone(),
                "vpns": inventory["vpns"].clone(),
                "apps": inventory["apps"].clone(),
                "gpus": gpus,
                "smartDisks": smart_disks,
                "batteries": batteries,
                "aiProxy": {
                    "proxyAddr": "127.0.0.1:3030",
                    "lastTokensPerSec": state.db.latest_metric("ai_tokens_per_sec"),
                    "samples": state.db.count_metric("ai_tokens_per_sec")
                },
                "egressTopology": egress["topology"].clone(),
                "egressByProcess": egress["byProcess"].clone(),
                "egressAccounting": egress["accounting"].clone(),
                "externalBaselines": baselines,
                "plur": {},
                "containers": monitors::containers::get_container_stats(),
                "outpostStats": {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cached_recomputes_only_after_ttl() {
        let slot: Mutex<Option<Cached>> = Mutex::new(None);
        let calls = AtomicUsize::new(0);
        let run = |ttl: Duration| {
            cached(&slot, ttl, || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                json!(format!("v{n}"))
            })
        };

        // First call computes and caches.
        assert_eq!(run(Duration::from_secs(60)), "v0");
        // Within TTL: returns cache, does NOT recompute.
        assert_eq!(run(Duration::from_secs(60)), "v0");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Zero TTL: the cached entry is immediately stale -> recompute.
        assert_eq!(run(Duration::from_secs(0)), "v1");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

fn main() {
    let db = Arc::new(database::Database::new(std::path::PathBuf::from("aetheris_telemetry.db")).expect("Failed to initialize local SQLite database"));
    let db_clone = db.clone();

    tauri::Builder::default()
        .setup(move |_app| {
            tauri::async_runtime::spawn(async move {
                server::start_server(db_clone).await;
            });
            // Upgrade the egress CIDR classifier from its coarse hardcoded
            // fallback to the live AWS/GCP published prefix lists, then refresh
            // periodically. The classifier is usable immediately regardless.
            tauri::async_runtime::spawn(async move {
                let client = reqwest::Client::new();
                loop {
                    monitors::cloud_ranges::refresh_from_network(&client).await;
                    tokio::time::sleep(std::time::Duration::from_secs(12 * 3600)).await;
                }
            });
            // Refresh live external baselines (LLM leaderboard ranks + git DORA)
            // every few hours; until the first fetch lands the UI shows an
            // explicit "unavailable" state rather than fabricated ranks.
            tauri::async_runtime::spawn(async move {
                let client = reqwest::Client::new();
                loop {
                    monitors::external_baselines::refresh(&client).await;
                    tokio::time::sleep(std::time::Duration::from_secs(3 * 3600)).await;
                }
            });
            // Refresh the docker container cache. A hung docker daemon must
            // never stall this loop forever, so each refresh is bounded by a
            // timeout; on expiry we log and try again next tick rather than
            // panicking or blocking indefinitely.
            tauri::async_runtime::spawn(async move {
                loop {
                    if tokio::time::timeout(std::time::Duration::from_secs(30), monitors::containers::refresh())
                        .await
                        .is_err()
                    {
                        eprintln!("monitors::containers::refresh() timed out after 30s; will retry next tick");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                }
            });
            // Try to load the eBPF egress probe once (no-op without the `ebpf`
            // feature or without root/CAP_BPF). When active it upgrades egress
            // accounting to true per-PID cumulative bytes.
            monitors::ebpf_egress::try_load();
            Ok(())
        })
        .manage(AppState {
            sys: Mutex::new(System::new_all()),
            networks: Mutex::new(Networks::new_with_refreshed_list()),
            disks: Mutex::new(Disks::new_with_refreshed_list()),
            components: Mutex::new(Components::new_with_refreshed_list()),
            db,
            caches: Caches::default(),
        })
        .invoke_handler(tauri::generate_handler![get_stats])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
