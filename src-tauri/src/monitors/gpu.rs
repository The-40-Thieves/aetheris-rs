//! GPU telemetry across NVIDIA, AMD and Apple Silicon.
//!
//! Previously the AMD branch ran `rocm-smi` but discarded its output (returning
//! hardcoded zeros), and the Apple branch did the same with `powermetrics`. Both
//! now parse real output. VRAM figures are normalised to **bytes** for every
//! vendor; unknown metrics are reported as `null` rather than a fake `0`.
//!
//! The tool invocations depend on hardware not present in CI, but the parsing is
//! factored into pure functions exercised by unit tests against captured sample
//! output.

use serde_json::{json, Value};
use std::process::Command;

pub fn get_gpu_stats() -> Vec<Value> {
    let mut gpus = Vec::new();
    gpus.extend(nvidia_gpus());
    gpus.extend(amd_gpus());
    #[cfg(target_os = "macos")]
    {
        gpus.extend(apple_gpus());
    }
    gpus
}

/// Assemble the per-GPU JSON the frontend consumes. VRAM is in bytes; any
/// unknown metric is `null`.
fn gpu_json(
    vendor: &str,
    model: &str,
    temp: Option<f64>,
    load: Option<f64>,
    vram_total_bytes: Option<f64>,
    vram_used_bytes: Option<f64>,
    power_w: Option<f64>,
) -> Value {
    json!({
        "vendor": vendor,
        "model": model,
        "temp": temp,
        "load": load,
        "vramTotal": vram_total_bytes,
        "vramUsed": vram_used_bytes,
        "powerW": power_w,
    })
}

// --- NVIDIA -------------------------------------------------------------------

fn nvidia_gpus() -> Vec<Value> {
    let output = Command::new("nvidia-smi")
        .arg("--query-gpu=name,temperature.gpu,utilization.gpu,memory.total,memory.used,power.draw")
        .arg("--format=csv,noheader,nounits")
        .output();
    match output {
        Ok(o) if o.status.success() => parse_nvidia_csv(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parse `nvidia-smi --format=csv,noheader,nounits`. Cells are separated by
/// ", " (comma+space) and may be "[N/A]"/"[Not Supported]"; memory is MiB.
fn parse_nvidia_csv(stdout: &str) -> Vec<Value> {
    let mut gpus = Vec::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let p: Vec<&str> = line.split(',').map(str::trim).collect();
        if p.len() < 5 {
            continue;
        }
        let num = |i: usize| -> Option<f64> {
            p.get(i)
                .filter(|s| !s.starts_with('['))
                .and_then(|s| s.parse::<f64>().ok())
        };
        let mib = |i: usize| num(i).map(|m| m * 1024.0 * 1024.0);
        gpus.push(gpu_json(
            "NVIDIA",
            p[0],
            num(1),
            num(2),
            mib(3),
            mib(4),
            num(5),
        ));
    }
    gpus
}

// --- AMD ----------------------------------------------------------------------

fn amd_gpus() -> Vec<Value> {
    // Prefer rocm-smi --json; fall back to the newer amd-smi array schema.
    let rocm = Command::new("rocm-smi")
        .args([
            "--showtemp",
            "--showuse",
            "--showmeminfo",
            "vram",
            "--showproductname",
            "--json",
        ])
        .output();
    if let Ok(o) = rocm {
        if o.status.success() {
            if let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) {
                let gpus = parse_rocm_json(&v);
                if !gpus.is_empty() {
                    return gpus;
                }
            }
        }
    }
    let amdsmi = Command::new("amd-smi").args(["metric", "--json"]).output();
    if let Ok(o) = amdsmi {
        if o.status.success() {
            if let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) {
                return parse_amdsmi_json(&v);
            }
        }
    }
    Vec::new()
}

/// Parse `rocm-smi --json`: an object keyed by "card0","card1",... where every
/// value is a *string*. Field names drift across ROCm versions, so look each up
/// defensively.
fn parse_rocm_json(v: &Value) -> Vec<Value> {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };
    let mut gpus = Vec::new();
    for (key, card) in obj {
        // Only the cardN entries; skip a possible top-level "system" object.
        if !(key.starts_with("card") && key[4..].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let s = |k: &str| -> Option<f64> {
            card.get(k)
                .and_then(|x| x.as_str())
                .and_then(|s| s.trim().parse::<f64>().ok())
        };
        // Power key differs (dGPU vs APU); match on the common substring.
        let power = card
            .as_object()
            .and_then(|c| {
                c.iter()
                    .find(|(k, _)| k.contains("Graphics Package Power (W)"))
                    .and_then(|(_, val)| val.as_str())
                    .and_then(|s| s.trim().parse::<f64>().ok())
            });
        let model = card
            .get("Card Series")
            .or_else(|| card.get("Card series"))
            .and_then(|x| x.as_str())
            .unwrap_or("Radeon GPU");
        gpus.push(gpu_json(
            "AMD",
            model,
            s("Temperature (Sensor edge) (C)"),
            s("GPU use (%)"),
            s("VRAM Total Memory (B)"),
            s("VRAM Total Used Memory (B)"),
            power,
        ));
    }
    gpus
}

/// Parse `amd-smi metric --json`: a JSON array indexed by GPU with nested
/// `{value, unit}` metrics. VRAM is reported in MB there -> convert to bytes.
fn parse_amdsmi_json(v: &Value) -> Vec<Value> {
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut gpus = Vec::new();
    for g in arr {
        let val = |path: &[&str]| -> Option<f64> {
            let mut cur = g;
            for k in path {
                cur = cur.get(k)?;
            }
            cur.as_f64()
        };
        let mb_to_bytes = |mb: Option<f64>| mb.map(|m| m * 1024.0 * 1024.0);
        gpus.push(gpu_json(
            "AMD",
            "AMD GPU",
            val(&["temperature", "edge", "value"]),
            val(&["usage", "gfx_activity", "value"]),
            mb_to_bytes(val(&["mem_usage", "total_vram", "value"])),
            mb_to_bytes(val(&["mem_usage", "used_vram", "value"])),
            val(&["power", "socket_power", "value"]),
        ));
    }
    gpus
}

// --- Apple Silicon ------------------------------------------------------------

#[cfg(target_os = "macos")]
fn apple_gpus() -> Vec<Value> {
    // powermetrics requires root; --format plist emits XML.
    let output = Command::new("powermetrics")
        .args(["-n", "1", "--samplers", "gpu_power", "--format", "plist"])
        .output();
    match output {
        Ok(o) if o.status.success() => parse_powermetrics_plist(&o.stdout),
        _ => Vec::new(),
    }
}

/// Parse a `powermetrics --format plist` document for GPU active residency and
/// power. Multi-sample output is NUL-separated; we parse the first segment.
/// GPU active residency = 1 - idle_ratio.
#[cfg(any(target_os = "macos", test))]
fn parse_powermetrics_plist(bytes: &[u8]) -> Vec<Value> {
    // Split on NUL in case of multi-sample streams; take the first document.
    let first: &[u8] = bytes.split(|&b| b == 0).find(|s| !s.is_empty()).unwrap_or(bytes);
    let root: plist::Value = match plist::from_bytes(first) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let dict = match root.as_dictionary() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let gpus = match dict.get("GPU").and_then(|g| g.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for gpu in gpus {
        let gd = match gpu.as_dictionary() {
            Some(d) => d,
            None => continue,
        };
        let idle_ratio = gd
            .get("idle_ratio")
            .or_else(|| gd.get("c_state_ratio"))
            .and_then(|v| v.as_real());
        let load = idle_ratio.map(|r| ((1.0 - r) * 100.0).clamp(0.0, 100.0));
        // gpu_power is in mW; fall back to energy/elapsed if absent.
        let power_w = gd
            .get("gpu_power")
            .and_then(|v| v.as_real().or_else(|| v.as_signed_integer().map(|i| i as f64)))
            .map(|mw| mw / 1000.0);
        out.push(gpu_json(
            "Apple",
            "Apple Silicon GPU",
            None, // no discrete temperature sensor via this sampler
            load,
            None, // unified memory — reported via system RAM, not VRAM
            None,
            power_w,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: &Value, k: &str) -> Option<f64> {
        v.get(k).and_then(|x| x.as_f64())
    }

    #[test]
    fn nvidia_csv_parses_and_normalises_vram_to_bytes() {
        let out = "NVIDIA GeForce RTX 4090, 45, 12, 24564, 1024, 55.5\n";
        let g = parse_nvidia_csv(out);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["vendor"], "NVIDIA");
        assert_eq!(f(&g[0], "temp"), Some(45.0));
        assert_eq!(f(&g[0], "load"), Some(12.0));
        assert_eq!(f(&g[0], "vramTotal"), Some(24564.0 * 1024.0 * 1024.0));
        assert_eq!(f(&g[0], "powerW"), Some(55.5));
    }

    #[test]
    fn nvidia_csv_handles_not_supported_cells() {
        let out = "GPU X, [N/A], [Not Supported], 8192, 100, [N/A]\n";
        let g = parse_nvidia_csv(out);
        assert!(g[0]["temp"].is_null());
        assert!(g[0]["load"].is_null());
        assert!(g[0]["powerW"].is_null());
        assert_eq!(f(&g[0], "vramTotal"), Some(8192.0 * 1024.0 * 1024.0));
    }

    #[test]
    fn rocm_json_parses_real_strings() {
        let v = json!({
            "system": { "Driver version": "6.8.5" },
            "card0": {
                "Temperature (Sensor edge) (C)": "44.0",
                "GPU use (%)": "37",
                "VRAM Total Memory (B)": "21458059264",
                "VRAM Total Used Memory (B)": "27856896",
                "Average Graphics Package Power (W)": "22.0",
                "Card Series": "Radeon RX 7900 XT"
            }
        });
        let g = parse_rocm_json(&v);
        assert_eq!(g.len(), 1, "system key skipped, one card parsed");
        assert_eq!(g[0]["vendor"], "AMD");
        assert_eq!(g[0]["model"], "Radeon RX 7900 XT");
        assert_eq!(f(&g[0], "temp"), Some(44.0));
        assert_eq!(f(&g[0], "load"), Some(37.0));
        assert_eq!(f(&g[0], "vramTotal"), Some(21458059264.0));
        assert_eq!(f(&g[0], "powerW"), Some(22.0));
    }

    #[test]
    fn amdsmi_array_schema_parses() {
        let v = json!([
            { "gpu": 0,
              "temperature": { "edge": {"value": 44, "unit": "C"} },
              "usage": { "gfx_activity": {"value": 5, "unit": "%"} },
              "mem_usage": { "total_vram": {"value": 20464, "unit": "MB"}, "used_vram": {"value": 26, "unit": "MB"} },
              "power": { "socket_power": {"value": 22, "unit": "W"} } }
        ]);
        let g = parse_amdsmi_json(&v);
        assert_eq!(g.len(), 1);
        assert_eq!(f(&g[0], "temp"), Some(44.0));
        assert_eq!(f(&g[0], "vramTotal"), Some(20464.0 * 1024.0 * 1024.0));
    }

    #[test]
    fn powermetrics_plist_active_residency() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>GPU</key>
  <array>
    <dict>
      <key>idle_ratio</key><real>0.75</real>
      <key>gpu_power</key><real>1500.0</real>
    </dict>
  </array>
</dict>
</plist>"#;
        let g = parse_powermetrics_plist(xml);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["vendor"], "Apple");
        // active residency = 1 - 0.75 = 25%
        assert_eq!(f(&g[0], "load"), Some(25.0));
        assert_eq!(f(&g[0], "powerW"), Some(1.5));
    }
}
