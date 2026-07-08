use serde_json::json;

pub fn get_battery_stats() -> Vec<serde_json::Value> {
    let mut batteries = Vec::new();

    if let Ok(manager) = starship_battery::Manager::new() {
        if let Ok(batt_iter) = manager.batteries() {
            for batt in batt_iter.flatten() {
                let soh = batt.state_of_health().value * 100.0;
                let soc = batt.state_of_charge().value * 100.0;
                let cycle_count = batt.cycle_count().unwrap_or(0);
                let time_to_empty = batt.time_to_empty().map(|t| t.value).unwrap_or(0.0);
                
                batteries.push(json!({
                    "vendor": batt.vendor().unwrap_or("Unknown"),
                    "model": batt.model().unwrap_or("Unknown"),
                    "stateOfHealth": soh,
                    "stateOfCharge": soc,
                    "cycleCount": cycle_count,
                    "timeToEmpty": time_to_empty,
                    "state": format!("{:?}", batt.state())
                }));
            }
        }
    }
    
    batteries
}
