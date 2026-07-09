//! Remaining-Useful-Life (RUL) projection for SSDs and batteries.
//!
//! Previously these used hardcoded velocities (50 GB/day writes, 1 cycle/day)
//! and the `telemetry` table was never written, so every projection was a
//! constant dressed up as a trend. Now each call records a throttled history
//! sample and, once enough history exists (>= ~7 days), projects End-of-Life
//! from the *actual* measured velocity. When history is too short we fall back
//! to a documented default and mark the result `"confidence":"low"` so the UI
//! can distinguish a real trend from a placeholder.

use crate::database::Database;
use serde_json::{json, Value};
use chrono::{Utc, Duration};

/// Throttle interval for history samples (30 min): fine enough for a per-day
/// velocity, coarse enough to keep the table small under 1 Hz polling.
const SAMPLE_INTERVAL_SECS: i64 = 1800;
/// History window to derive velocity from.
const SSD_WINDOW_DAYS: i64 = 30;
const BATTERY_WINDOW_DAYS: i64 = 60;
/// Minimum span of real history before we trust a measured velocity.
const MIN_HISTORY_DAYS: f64 = 7.0;
/// Documented fallback SSD write rate used only when history is too short.
const DEFAULT_SSD_WRITE_BYTES_PER_DAY: f64 = 50.0 * 1024.0 * 1024.0 * 1024.0;
/// Cap projected life so display dates and chrono math stay sane (~100 yrs).
const MAX_PROJECTION_DAYS: f64 = 36_500.0;

pub fn calculate_ssd_rul(db: &Database, model: &str, bytes_written: f64) -> Value {
    // Record a throttled history sample so a velocity can be derived over time.
    let _ = db.insert_metric_if_stale("ssd_bytes_written", bytes_written, model, SAMPLE_INTERVAL_SECS);

    let conn = db.conn.lock().unwrap();
    let tbw_rating: f64 = conn
        .query_row(
            "SELECT tbw_rating_tb FROM ssd_endurance WHERE model_match = ?1 OR model_match = 'Generic 1TB' ORDER BY model_match DESC LIMIT 1",
            rusqlite::params![model],
            |row| row.get(0),
        )
        .unwrap_or(300.0);
    drop(conn);

    let max_bytes = tbw_rating * 1024.0 * 1024.0 * 1024.0 * 1024.0;
    let remaining_bytes = (max_bytes - bytes_written).max(0.0);
    let health_percent = if max_bytes > 0.0 {
        (remaining_bytes / max_bytes * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    // Prefer a velocity measured from real history; else a documented default.
    let series = db.metric_series("ssd_bytes_written", model, SSD_WINDOW_DAYS);
    let (daily_velocity, confidence) = match increasing_velocity_per_day(&series) {
        Some((v, days)) if days >= MIN_HISTORY_DAYS && v > 0.0 => (v, "high"),
        _ => (DEFAULT_SSD_WRITE_BYTES_PER_DAY, "low"),
    };

    let days_remaining = (remaining_bytes / daily_velocity).clamp(0.0, MAX_PROJECTION_DAYS);
    let eol_date = Utc::now() + Duration::days(days_remaining as i64);

    json!({
        "model": model,
        "tbwRatingTB": tbw_rating,
        "healthPercent": health_percent,
        "estimatedEndOfLife": eol_date.to_rfc3339(),
        "dailyWriteBytes": daily_velocity,
        "confidence": confidence,
        "historySamples": series.len(),
    })
}

pub fn calculate_battery_rul(db: &Database, context: &str, current_soh: f64, cycle_count: i32) -> Value {
    let _ = db.insert_metric_if_stale("battery_soh", current_soh, context, SAMPLE_INTERVAL_SECS);

    let series = db.metric_series("battery_soh", context, BATTERY_WINDOW_DAYS);

    // Real projection: SOH decline per day from history -> days until 80%.
    let (days_remaining, confidence) = match declining_velocity_per_day(&series) {
        Some((soh_per_day, days))
            if days >= MIN_HISTORY_DAYS && soh_per_day > 0.0 && current_soh > 80.0 =>
        {
            ((current_soh - 80.0) / soh_per_day, "high")
        }
        // Fallback: cycle-based extrapolation assuming ~1 cycle/day.
        _ => (fallback_battery_days(current_soh, cycle_count), "low"),
    };

    let days_remaining = days_remaining.clamp(0.0, MAX_PROJECTION_DAYS);
    let eol_date = Utc::now() + Duration::days(days_remaining as i64);

    json!({
        "healthPercent": current_soh,
        "cyclesRemainingTo80": days_remaining, // ~1 cycle/day in the fallback model
        "estimatedEndOfLife": eol_date.to_rfc3339(),
        "confidence": confidence,
        "historySamples": series.len(),
    })
}

/// Cycle-based fallback (the old model), in *days* assuming ~1 cycle/day.
fn fallback_battery_days(current_soh: f64, cycle_count: i32) -> f64 {
    if current_soh < 80.0 {
        return 0.0;
    }
    let wear_per_cycle = (100.0 - current_soh) / (cycle_count as f64).max(1.0);
    let wear_remaining_to_80 = current_soh - 80.0;
    if wear_per_cycle > 0.0 {
        wear_remaining_to_80 / wear_per_cycle
    } else {
        500.0 // default buffer when we can't infer wear yet
    }
}

/// Velocity of an *increasing* counter (bytes written) in units/day, plus the
/// span of history in days. None if <2 samples, non-positive span, or the
/// counter did not advance (e.g. a controller reset).
fn increasing_velocity_per_day(series: &[(i64, f64)]) -> Option<(f64, f64)> {
    let (t0, v0) = series.first()?;
    let (t1, v1) = series.last()?;
    let days = (t1 - t0) as f64 / 86_400.0;
    if days <= 0.0 {
        return None;
    }
    let dv = v1 - v0;
    if dv <= 0.0 {
        return None;
    }
    Some((dv / days, days))
}

/// Velocity of a *declining* value (battery SOH) in units/day, plus span days.
fn declining_velocity_per_day(series: &[(i64, f64)]) -> Option<(f64, f64)> {
    let (t0, v0) = series.first()?;
    let (t1, v1) = series.last()?;
    let days = (t1 - t0) as f64 / 86_400.0;
    if days <= 0.0 {
        return None;
    }
    let decline = v0 - v1;
    if decline <= 0.0 {
        return None;
    }
    Some((decline / days, days))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increasing_velocity_needs_advancing_counter() {
        // 10 GB written over 5 days -> 2 GB/day.
        let gb = 1024.0 * 1024.0 * 1024.0;
        let series = vec![(0, 0.0), (5 * 86_400, 10.0 * gb)];
        let (v, days) = increasing_velocity_per_day(&series).unwrap();
        assert!((days - 5.0).abs() < 1e-6);
        assert!((v - 2.0 * gb).abs() < 1.0);
        // Flat/reset counter -> None (no fabricated velocity).
        assert!(increasing_velocity_per_day(&[(0, 100.0), (86_400, 100.0)]).is_none());
        assert!(increasing_velocity_per_day(&[(0, 100.0), (86_400, 50.0)]).is_none());
        assert!(increasing_velocity_per_day(&[(0, 5.0)]).is_none());
    }

    #[test]
    fn declining_velocity_for_soh() {
        // SOH 100 -> 96 over 10 days -> 0.4 %/day; to reach 80 from 96 = 40 days.
        let series = vec![(0, 100.0), (10 * 86_400, 96.0)];
        let (per_day, days) = declining_velocity_per_day(&series).unwrap();
        assert!((per_day - 0.4).abs() < 1e-6);
        assert!((days - 10.0).abs() < 1e-6);
        assert!(declining_velocity_per_day(&[(0, 90.0), (86_400, 92.0)]).is_none()); // rising
    }

    #[test]
    fn ssd_rul_marks_confidence_and_writes_history() {
        let db = Database::new(std::path::PathBuf::from(":memory:")).unwrap();
        // First call: no history -> low confidence, fallback velocity, 1 sample.
        let r = calculate_ssd_rul(&db, "Generic 1TB", 100.0 * 1024.0 * 1024.0 * 1024.0);
        assert_eq!(r["confidence"], "low");
        assert!(r["healthPercent"].as_f64().unwrap() > 0.0);
        assert_eq!(db.count_metric("ssd_bytes_written"), 1, "history sample was recorded");
    }

    #[test]
    fn battery_fallback_when_no_history() {
        let db = Database::new(std::path::PathBuf::from(":memory:")).unwrap();
        let r = calculate_battery_rul(&db, "TestVendor TestModel", 95.0, 100);
        assert_eq!(r["confidence"], "low");
        assert_eq!(r["healthPercent"], 95.0);
        assert_eq!(db.count_metric("battery_soh"), 1);
    }
}
