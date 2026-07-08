use crate::database::Database;
use serde_json::{json, Value};
use chrono::{Utc, Duration};
use rusqlite::OptionalExtension;

pub fn calculate_ssd_rul(db: &Database, model: &str, bytes_written: f64) -> Value {
    let conn = db.conn.lock().unwrap();
    
    // Look up TBW rating (fallback to Generic 1TB if missing)
    let tbw_rating: f64 = conn.query_row(
        "SELECT tbw_rating_tb FROM ssd_endurance WHERE model_match = ?1 OR model_match = 'Generic 1TB' ORDER BY model_match DESC LIMIT 1",
        rusqlite::params![model],
        |row| row.get(0),
    ).unwrap_or(300.0);

    let max_bytes = tbw_rating * 1024.0 * 1024.0 * 1024.0 * 1024.0;
    let remaining_bytes = max_bytes - bytes_written;
    let health_percent = (remaining_bytes / max_bytes) * 100.0;

    // Ideally, we fetch the historical delta of bytes_written over the last 30 days to get a bytes/day velocity.
    // For this prototype, we mock a daily write velocity of 50 GB/day to demonstrate the projection.
    let daily_write_velocity = 50.0 * 1024.0 * 1024.0 * 1024.0; 
    let days_remaining = remaining_bytes / daily_write_velocity;
    
    let eol_date = Utc::now() + Duration::days(days_remaining as i64);

    json!({
        "model": model,
        "tbwRatingTB": tbw_rating,
        "healthPercent": health_percent,
        "estimatedEndOfLife": eol_date.to_rfc3339()
    })
}

pub fn calculate_battery_rul(db: &Database, current_soh: f64, cycle_count: i32) -> Value {
    // Standard lithium ion typically hits 80% SOH at 500-1000 cycles.
    // We project when it will hit 80%.
    
    let cycles_remaining = if current_soh >= 80.0 {
        // Rough linear extrapolation assuming 80% at 1000 cycles.
        let wear_per_cycle = (100.0 - current_soh) / (cycle_count as f64).max(1.0);
        let wear_remaining_to_80 = current_soh - 80.0;
        if wear_per_cycle > 0.0 {
            wear_remaining_to_80 / wear_per_cycle
        } else {
            500.0 // Default buffer
        }
    } else {
        0.0 // Already reached end of ideal life
    };

    // Assuming 1 cycle per day velocity
    let eol_date = Utc::now() + Duration::days(cycles_remaining as i64);

    json!({
        "healthPercent": current_soh,
        "cyclesRemainingTo80": cycles_remaining,
        "estimatedEndOfLife": eol_date.to_rfc3339()
    })
}
