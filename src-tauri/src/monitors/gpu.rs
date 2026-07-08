use serde_json::json;
use std::process::Command;

pub fn get_gpu_stats() -> Vec<serde_json::Value> {
    let mut gpus = Vec::new();

    // Try NVIDIA
    if let Ok(output) = Command::new("nvidia-smi")
        .arg("--query-gpu=name,temperature.gpu,utilization.gpu,memory.total,memory.used")
        .arg("--format=csv,noheader,nounits")
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split(", ").collect();
                if parts.len() >= 5 {
                    gpus.push(json!({
                        "vendor": "NVIDIA",
                        "model": parts[0],
                        "temp": parts[1].parse::<f64>().unwrap_or(0.0),
                        "load": parts[2].parse::<f64>().unwrap_or(0.0),
                        "vramTotal": parts[3].parse::<f64>().unwrap_or(0.0),
                        "vramUsed": parts[4].parse::<f64>().unwrap_or(0.0),
                    }));
                }
            }
        }
    }

    // Try AMD
    if let Ok(output) = Command::new("rocm-smi")
        .arg("--showtemp")
        .arg("--showuse")
        .arg("--showmeminfo")
        .arg("vram")
        .output()
    {
        if output.status.success() {
            // Very basic rocm parsing, in a real app would use JSON flag if available
            gpus.push(json!({
                "vendor": "AMD",
                "model": "Radeon GPU", // Would extract via rocm-smi --showproductname
                "temp": 0.0,
                "load": 0.0,
                "vramTotal": 0.0,
                "vramUsed": 0.0,
            }));
        }
    }

    // Try Apple Silicon (Mac)
    #[cfg(target_os = "macos")]
    {
        // powermetrics requires root
        if let Ok(output) = Command::new("powermetrics")
            .args(["-n", "1", "--samplers", "gpu_power"])
            .output()
        {
            if output.status.success() {
                gpus.push(json!({
                    "vendor": "Apple",
                    "model": "Apple Silicon GPU",
                    "temp": 0.0,
                    "load": 0.0,
                    "vramTotal": 0.0, // Unified memory
                    "vramUsed": 0.0,
                }));
            }
        }
    }

    gpus
}
