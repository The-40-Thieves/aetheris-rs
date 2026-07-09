use rusqlite::{Connection, OptionalExtension, Result};
use std::sync::Mutex;
use std::path::PathBuf;

pub struct Database {
    pub conn: Mutex<Connection>,
}

impl Database {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        
        // Initialize tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS telemetry (
                id INTEGER PRIMARY KEY,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                metric_type TEXT NOT NULL,
                value REAL NOT NULL,
                context TEXT
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS ssd_endurance (
                model_match TEXT PRIMARY KEY,
                tbw_rating_tb REAL NOT NULL
            )",
            [],
        )?;

        // Seed some known SSD TBWs for Health / RUL calculation
        Self::seed_tbw(&conn)?;

        Ok(Database {
            conn: Mutex::new(conn),
        })
    }

    /// Append one telemetry sample. Used by the AI proxy (tokens/sec) and the
    /// hardware monitors (SSD bytes written, battery SOH/cycles) so the RUL
    /// analytics can compute real velocities from history instead of constants.
    /// `context` is a free-form string (we store JSON) identifying the subject
    /// (disk model, battery vendor/model, inference engine).
    pub fn insert_metric(&self, metric_type: &str, value: f64, context: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO telemetry (metric_type, value, context) VALUES (?1, ?2, ?3)",
            rusqlite::params![metric_type, value, context],
        )
    }

    /// Append a telemetry sample only if the newest existing sample for the same
    /// (metric_type, context) is at least `min_interval_secs` old (or none
    /// exists). This bounds table growth: get_stats can be polled every second,
    /// but a bytes-written / SOH history sampled every ~30 min is plenty to
    /// derive a per-day velocity, and avoids ~86k rows/day/subject. Returns
    /// whether a row was inserted.
    pub fn insert_metric_if_stale(
        &self,
        metric_type: &str,
        value: f64,
        context: &str,
        min_interval_secs: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let age_secs: Option<i64> = conn
            .query_row(
                "SELECT CAST(strftime('%s','now') AS INTEGER) \
                        - CAST(strftime('%s', timestamp) AS INTEGER) \
                 FROM telemetry WHERE metric_type = ?1 AND context = ?2 \
                 ORDER BY id DESC LIMIT 1",
                rusqlite::params![metric_type, context],
                |row| row.get(0),
            )
            .optional()?;
        let should_insert = age_secs.is_none_or(|age| age >= min_interval_secs);
        if should_insert {
            conn.execute(
                "INSERT INTO telemetry (metric_type, value, context) VALUES (?1, ?2, ?3)",
                rusqlite::params![metric_type, value, context],
            )?;
        }
        Ok(should_insert)
    }

    /// Time series of (unix_epoch_secs, value) for a metric+context within the
    /// last `within_days`, oldest first. Used to compute real degradation
    /// velocities for RUL projection.
    pub fn metric_series(
        &self,
        metric_type: &str,
        context: &str,
        within_days: i64,
    ) -> Vec<(i64, f64)> {
        let conn = self.conn.lock().unwrap();
        let window = format!("-{within_days} days");
        let mut stmt = match conn.prepare(
            "SELECT CAST(strftime('%s', timestamp) AS INTEGER), value \
             FROM telemetry \
             WHERE metric_type = ?1 AND context = ?2 AND timestamp >= datetime('now', ?3) \
             ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(
            rusqlite::params![metric_type, context, window],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
        );
        match rows {
            Ok(iter) => iter.flatten().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Most recent recorded value for a metric type, if any.
    pub fn latest_metric(&self, metric_type: &str) -> Option<f64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM telemetry WHERE metric_type = ?1 ORDER BY id DESC LIMIT 1",
            rusqlite::params![metric_type],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Number of recorded samples for a metric type.
    pub fn count_metric(&self, metric_type: &str) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM telemetry WHERE metric_type = ?1",
            rusqlite::params![metric_type],
            |row| row.get(0),
        )
        .unwrap_or(0)
    }

    fn seed_tbw(conn: &Connection) -> Result<()> {
        let models = vec![
            ("Samsung SSD 860 EVO 500GB", 300.0),
            ("Samsung SSD 970 EVO Plus 1TB", 600.0),
            ("Samsung SSD 980 PRO 2TB", 1200.0),
            ("WD Blue SN550 1TB", 600.0),
            ("Crucial MX500 1TB", 360.0),
            ("Generic 1TB", 300.0), 
        ];

        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO ssd_endurance (model_match, tbw_rating_tb) VALUES (?1, ?2)"
        )?;

        for (model, tbw) in models {
            stmt.execute(rusqlite::params![model, tbw])?;
        }

        Ok(())
    }
}
