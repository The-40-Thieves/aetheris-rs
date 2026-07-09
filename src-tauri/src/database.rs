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
