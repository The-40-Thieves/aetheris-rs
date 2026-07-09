//! SMART disk endurance reader via `smartctl -j`.
//!
//! Previously this only read the NVMe `data_units_written` field, so every
//! SATA/ATA SSD silently reported `bytesWritten: 0.0`. It now branches on device
//! class and, for SATA, reads the `Total_LBAs_Written` SMART attribute (id 241,
//! with vendor variants) — the byte-written figure the RUL projection depends on.

use serde_json::{json, Value};
use std::process::Command;

pub fn get_smart_data() -> Vec<Value> {
    let mut disks = Vec::new();

    // Enumerate devices with `smartctl --scan`.
    if let Ok(output) = Command::new("smartctl").arg("--scan").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if let Some(dev_path) = line.split_whitespace().next() {
                    if let Ok(json_out) = Command::new("smartctl").args(["-a", dev_path, "-j"]).output() {
                        if let Ok(data) = serde_json::from_slice::<Value>(&json_out.stdout) {
                            let info = parse_smart_json(&data);
                            disks.push(json!({
                                "device": dev_path,
                                "model": info.model,
                                "powerOnHours": info.power_on_hours,
                                "bytesWritten": info.bytes_written,
                                "passed": info.passed,
                                "isSsd": info.is_ssd,
                            }));
                        }
                    }
                }
            }
        }
    }

    disks
}

struct SmartInfo {
    model: String,
    power_on_hours: f64,
    bytes_written: f64,
    passed: bool,
    is_ssd: bool,
}

/// Pure parser over a `smartctl -a -j` JSON object. Branches NVMe vs SATA/ATA and
/// extracts total bytes written using the correct per-class / per-vendor units.
fn parse_smart_json(data: &Value) -> SmartInfo {
    let model = data["model_name"].as_str().unwrap_or("Unknown").to_string();
    let power_on_hours = data["power_on_time"]["hours"].as_f64().unwrap_or(0.0);
    let passed = data["smart_status"]["passed"].as_bool().unwrap_or(true);
    // rotation_rate == 0 means SSD/flash; absent means unknown (not necessarily SSD).
    let is_ssd = data["rotation_rate"].as_i64() == Some(0)
        || data.get("nvme_smart_health_information_log").is_some();

    let bytes_written = if let Some(duw) =
        data["nvme_smart_health_information_log"]["data_units_written"].as_f64()
    {
        // NVMe: each data unit is 1000 * 512 bytes; controller always normalises
        // to 512-byte units regardless of logical block size.
        duw * 1000.0 * 512.0
    } else if let Some(table) = data["ata_smart_attributes"]["table"].as_array() {
        let logical_block_size = data["logical_block_size"].as_f64().unwrap_or(512.0);
        sata_bytes_written(table, logical_block_size)
    } else {
        0.0 // SCSI/SAS or a drive exposing no write attribute (e.g. many HDDs)
    };

    SmartInfo { model, power_on_hours, bytes_written, passed, is_ssd }
}

/// Compute host bytes written from a SATA `ata_smart_attributes.table`.
/// Handles the common `Total_LBAs_Written` (id 241/246) plus Intel/Solidigm
/// `Host_Writes_32MiB` and Kingston `Lifetime_Writes_GiB` encodings, and the
/// Intel gotcha where id 241 is *named* Total_LBAs_Written but is really in
/// 32 MiB units (detected by id 225 carrying an equal raw value).
fn sata_bytes_written(table: &[Value], logical_block_size: f64) -> f64 {
    const MIB_32: f64 = 32.0 * 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let raw = |id: u64| -> Option<f64> {
        table
            .iter()
            .find(|e| e["id"].as_u64() == Some(id))
            .and_then(|e| e["raw"]["value"].as_f64())
    };
    let find = |pred: &dyn Fn(u64, &str) -> bool| -> Option<(f64, String)> {
        table.iter().find_map(|e| {
            let id = e["id"].as_u64()?;
            let name = e["name"].as_str().unwrap_or("");
            if pred(id, name) {
                Some((e["raw"]["value"].as_f64()?, name.to_string()))
            } else {
                None
            }
        })
    };

    // Intel "Host_Writes_32MiB" (id 225 or 241) -> 32 MiB units.
    if let Some((v, _)) = find(&|_, name| name.eq_ignore_ascii_case("Host_Writes_32MiB")) {
        return v * MIB_32;
    }
    // Kingston "Lifetime_Writes_GiB" / "Total_Writes_GiB" -> GiB units.
    if let Some((v, _)) = find(&|_, name| {
        name.eq_ignore_ascii_case("Lifetime_Writes_GiB") || name.eq_ignore_ascii_case("Total_Writes_GiB")
    }) {
        return v * GIB;
    }
    // Standard Total_LBAs_Written (id 241, or 246 on Micron) -> LBAs.
    if let Some((v, _)) = find(&|id, name| {
        (id == 241 || id == 246) && name.eq_ignore_ascii_case("Total_LBAs_Written")
    }) {
        // Gotcha: some Intel drives label id 241 "Total_LBAs_Written" but the raw
        // value is really 32 MiB units, mirrored on id 225. If id 225 exists with
        // the same raw value, trust the 32 MiB interpretation.
        if let Some(v225) = raw(225) {
            if (v225 - v).abs() < 1.0 {
                return v * MIB_32;
            }
        }
        return v * logical_block_size;
    }

    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvme_data_units_written() {
        let data = json!({
            "model_name": "Samsung SSD 980 PRO 2TB",
            "rotation_rate": 0,
            "smart_status": { "passed": true },
            "power_on_time": { "hours": 1234 },
            "nvme_smart_health_information_log": { "data_units_written": 1_000_000 }
        });
        let info = parse_smart_json(&data);
        // 1,000,000 * 1000 * 512 = 512,000,000,000 bytes
        assert_eq!(info.bytes_written, 512_000_000_000.0);
        assert!(info.is_ssd);
        assert_eq!(info.model, "Samsung SSD 980 PRO 2TB");
        assert_eq!(info.power_on_hours, 1234.0);
    }

    #[test]
    fn parses_sata_total_lbas_written() {
        // The previously-broken case: a SATA SSD with no NVMe log.
        let data = json!({
            "model_name": "Samsung SSD 860 EVO 500GB",
            "rotation_rate": 0,
            "logical_block_size": 512,
            "smart_status": { "passed": true },
            "ata_smart_attributes": { "table": [
                { "id": 9,   "name": "Power_On_Hours",     "raw": { "value": 5000 } },
                { "id": 241, "name": "Total_LBAs_Written", "raw": { "value": 216456537880u64 } }
            ]}
        });
        let info = parse_smart_json(&data);
        // 216456537880 * 512 = 110,825,747,394,560 bytes (was 0.0 before this fix)
        assert_eq!(info.bytes_written, 216_456_537_880.0 * 512.0);
        assert!(info.is_ssd);
    }

    #[test]
    fn parses_intel_host_writes_32mib() {
        let data = json!({
            "model_name": "INTEL SSDSC2KB240G8",
            "logical_block_size": 512,
            "ata_smart_attributes": { "table": [
                { "id": 225, "name": "Host_Writes_32MiB", "raw": { "value": 1000 } },
                { "id": 241, "name": "Total_LBAs_Written", "raw": { "value": 1000 } }
            ]}
        });
        // id 225/241 equal -> 32 MiB units: 1000 * 32MiB, not 1000 * 512.
        assert_eq!(parse_smart_json(&data).bytes_written, 1000.0 * 32.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn hdd_without_write_attribute_reports_zero_not_error() {
        let data = json!({
            "model_name": "WDC WD20EFRX",
            "rotation_rate": 5400,
            "ata_smart_attributes": { "table": [
                { "id": 9, "name": "Power_On_Hours", "raw": { "value": 10000 } }
            ]}
        });
        let info = parse_smart_json(&data);
        assert_eq!(info.bytes_written, 0.0);
        assert!(!info.is_ssd);
    }
}
