use rusqlite::{Connection, Result};
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
