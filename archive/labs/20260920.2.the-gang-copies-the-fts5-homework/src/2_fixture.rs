use hafley_observe::{CountRecorder, SpanCounts};
use rusqlite::{Connection, Result};
use tracing_subscriber::prelude::*;

/// Source table plus the three hooks that stage OLD and NEW images, the same
/// positional shape src/1_maintenance.rs:388 generates for the engine.
pub const SCHEMA: &str = "
CREATE TABLE base(k INTEGER PRIMARY KEY, g INTEGER NOT NULL, v INTEGER NOT NULL);
";

pub const HOOKS: &str = "
CREATE TRIGGER base_ai AFTER INSERT ON base BEGIN
  INSERT INTO totals(__ivm_adding,__ivm_g,__ivm_v) VALUES(1,NEW.g,NEW.v);
END;
CREATE TRIGGER base_ad AFTER DELETE ON base BEGIN
  INSERT INTO totals(__ivm_adding,__ivm_g,__ivm_v) VALUES(0,OLD.g,OLD.v);
END;
CREATE TRIGGER base_au AFTER UPDATE ON base BEGIN
  INSERT INTO totals(__ivm_adding,__ivm_g,__ivm_v) VALUES(0,OLD.g,OLD.v);
  INSERT INTO totals(__ivm_adding,__ivm_g,__ivm_v) VALUES(1,NEW.g,NEW.v);
END;
";

pub fn connect(arguments: &str) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;")?;
    crate::vtab::register(&db)?;
    db.execute_batch(SCHEMA)?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE totals USING lab_ivm({arguments});"
    ))?;
    db.execute_batch(HOOKS)?;
    Ok(db)
}

/// Row count at which the byte cap fires, so a test names rows and not bytes.
pub fn cap_for(rows: usize) -> String {
    format!("cap={}", rows * crate::pending::STAGED_BYTES)
}

/// Full recompute from the source table. The oracle every correctness case ends on.
pub fn oracle(db: &Connection) -> Result<Vec<(i64, i64, i64)>> {
    db.prepare("SELECT g,COUNT(*),SUM(v) FROM base GROUP BY g ORDER BY g")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

pub fn arrangement(db: &Connection) -> Result<Vec<(i64, i64, i64)>> {
    db.prepare("SELECT g,n,s FROM totals ORDER BY g")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

/// Runs `body` under a capture layer and returns its span counts alongside its
/// value. Counts are deterministic, so no repetition or tolerance is needed.
pub fn counted<T>(body: impl FnOnce() -> T) -> (T, SpanCounts) {
    let (recorder, layer) = CountRecorder::new();
    let subscriber = tracing_subscriber::registry().with(layer);
    let value = tracing::subscriber::with_default(subscriber, body);
    (value, recorder.counts())
}

/// One transaction, one statement per row: the shape a client loop produces.
pub fn seed_per_row(db: &Connection, rows: usize) -> Result<()> {
    db.execute_batch("BEGIN")?;
    let mut insert = db.prepare("INSERT INTO base(k,g,v) VALUES(?1,?2,?3)")?;
    for k in 0..rows {
        insert.execute(rusqlite::params![k as i64, (k % 4) as i64, k as i64])?;
    }
    drop(insert);
    db.execute_batch("COMMIT")
}

/// One transaction, one statement: the shape a bulk load produces.
pub fn seed_bulk(db: &Connection, rows: usize) -> Result<()> {
    db.execute_batch("BEGIN")?;
    db.execute(
        "WITH RECURSIVE k(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM k WHERE i+1<?1)
         INSERT INTO base(k,g,v) SELECT i,i%4,i FROM k",
        [rows as i64],
    )?;
    db.execute_batch("COMMIT")
}
