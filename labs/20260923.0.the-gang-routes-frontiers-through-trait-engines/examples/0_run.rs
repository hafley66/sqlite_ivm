use lab_20260923_0::{Change, FrontierEngine, Plan, RustEngine, SourceRow, SqliteEngine};
use std::{env, error::Error, path::PathBuf, time::Instant};

fn row(source: u8, id: i64, a: i64, b: i64) -> Change {
    Change {
        source,
        row: SourceRow {
            id,
            cells: vec![a, b],
        },
        weight: 1,
    }
}

fn plan() -> Plan {
    Plan::JoinUnion {
        left: 0,
        right: 1,
        direct: 2,
        left_key: 1,
        right_key: 0,
        left_output: 0,
        right_output: 1,
        direct_output: [0, 1],
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let observe = hafley_observe::Config::from_env(
        "engine_iso_lab",
        env!("CARGO_PKG_VERSION"),
        "warn",
        false,
    )?;
    hafley_observe::init(observe)?;
    let mode = env::args().nth(1).unwrap_or_else(|| "rust".to_string());
    let batch = [
        row(0, 1, 1, 10),
        row(0, 2, 1, 20),
        row(1, 1, 10, 100),
        row(1, 2, 20, 100),
        row(2, 1, 3, 300),
    ];
    if mode == "rust" {
        let mut engine = RustEngine::new();
        engine.install(plan())?;
        let started = Instant::now();
        let frontier = engine.apply(&batch)?;
        println!(
            "mode=rust frontier={} inputs={} outputs={} elapsed_ns={}",
            frontier.id,
            batch.len(),
            frontier.changes.len(),
            started.elapsed().as_nanos()
        );
        println!("snapshot={:?}", engine.snapshot()?);
        println!(
            "rust_peak_rss_bytes={:?}",
            hafley_observe::process_sample().peak_rss_bytes
        );
    } else if mode == "sqlite" {
        let path = env::var_os("ENGINE_ISO_DB").map(PathBuf::from);
        let mut engine = match &path {
            Some(path) => SqliteEngine::open(path)?,
            None => SqliteEngine::memory()?,
        };
        if path.is_some() {
            engine
                .connection()
                .execute_batch("PRAGMA journal_mode=WAL;")?;
        }
        engine.install(plan())?;
        let started = Instant::now();
        let frontier = engine.apply(&batch)?;
        println!(
            "mode=sqlite frontier={} inputs={} outputs={} elapsed_ns={}",
            frontier.id,
            batch.len(),
            frontier.changes.len(),
            started.elapsed().as_nanos()
        );
        println!("snapshot={:?}", engine.snapshot()?);
        let db = engine.connection();
        let count = |kind| {
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type=?1",
                [kind],
                |row| row.get::<_, i64>(0),
            )
        };
        println!(
            "objects tables={} indexes={} triggers={}",
            count("table")?,
            count("index")?,
            count("trigger")?
        );
        println!("maintenance_sql_bytes={}", engine.maintenance_sql()?.len());
        println!("maintenance_plan={:?}", engine.explain_maintenance()?);
        println!("callbacks={:?}", sqlite_ext::counts(db, "iso_watch"));
        println!(
            "sqlite_memory={:?}",
            hafley_observe::sqlite_memory::sample(db)
        );
        println!(
            "rust_peak_rss_bytes={:?}",
            hafley_observe::process_sample().peak_rss_bytes
        );
        if let Some(path) = path {
            println!("database_bytes={}", std::fs::metadata(&path)?.len());
            println!(
                "wal_bytes={}",
                std::fs::metadata(path.with_extension("db-wal"))
                    .map(|meta| meta.len())
                    .unwrap_or(0)
            );
        }
    } else {
        return Err(format!("mode must be rust or sqlite, got {mode}").into());
    }
    Ok(())
}
