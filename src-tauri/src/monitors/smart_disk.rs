use serde_json::{json, Value};
use std::process::Command;

pub fn get_smart_data() -> Vec<Value> {
    let mut disks = Vec::new();
    
    // Attempt to list devices with smartctl --scan
    if let Ok(output) = Command::new("smartctl").arg("--scan").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if let Some(dev_path) = line.split_whitespace().next() {
                    // Fetch json data for each device
                    if let Ok(json_out) = Command::new("smartctl").args(["-a", dev_path, "-j"]).output() {
                        if let Ok(data) = serde_json::from_slice::<Value>(&json_out.stdout) {
                            let model_name = data["model_name"].as_str().unwrap_or("Unknown");
                            let power_on_hours = data["power_on_time"]["hours"].as_f64().unwrap_or(0.0);
                            
                            // Rough NVMe TBW calculation from data_units_written (in 1000s of 512 byte sectors)
                            let bytes_written = data["nvme_smart_health_information_log"]["data_units_written"]
                                .as_f64().map(|v| v * 512.0 * 1000.0).unwrap_or(0.0); 
                            
                            disks.push(json!({
                                "device": dev_path,
                                "model": model_name,
                                "powerOnHours": power_on_hours,
                                "bytesWritten": bytes_written,
                                "passed": data["smart_status"]["passed"].as_bool().unwrap_or(true),
                            }));
                        }
                    }
                }
            }
        }
    }
    
    disks
}
