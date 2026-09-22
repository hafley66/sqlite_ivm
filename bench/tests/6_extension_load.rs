use rusqlite::{Connection, Result};
use std::path::PathBuf;

/// The gate supplies the freshly built extension. Direct invocations look
/// beside the test executable's profile directory, matching the bench runner.
fn artifact() -> PathBuf {
    if let Some(path) = std::env::var_os("IVM_EXTENSION") {
        return PathBuf::from(path);
    }
    let name = if cfg!(target_os = "macos") {
        "libsqlite_ivm.dylib"
    } else if cfg!(target_os = "linux") {
        "libsqlite_ivm.so"
    } else {
        "sqlite_ivm.dll"
    };
    std::env::current_exe().expect("test executable")
        .parent().expect("deps directory")
        .parent().expect("profile directory").join(name)
}

fn open_loaded() -> Result<Connection> {
    let path = artifact();
    assert!(path.is_file(), "extension missing at {}; build with scripts/0_build.sh and set IVM_EXTENSION", path.display());
    let db = Connection::open_in_memory()?;
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(&path, Some("sqlite3_extension_init"))?;
        db.load_extension_disable()?;
    }
    Ok(db)
}

#[test]
fn extension_loads_through_its_entry_point() -> Result<()> {
    let _db = open_loaded()?;
    Ok(())
}

#[test]
fn loaded_extension_registers_the_create_function() -> Result<()> {
    let db = open_loaded()?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
        CREATE TABLE items (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, amount INTEGER NOT NULL)",
    )?;
    let installed: String = db.query_row(
        "SELECT sqlite_ivm_create(?1, ?2)",
        [
            "probe_view",
            "SELECT group_id AS g, COUNT(*) AS n, SUM(amount) AS s
             FROM items GROUP BY group_id",
        ],
        |r| r.get(0),
    )?;
    assert_eq!(installed, "probe_view");
    Ok(())
}

#[test]
fn loaded_extension_maintains_a_view_end_to_end() -> Result<()> {
    let db = open_loaded()?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
        CREATE TABLE items (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, amount INTEGER NOT NULL);",
    )?;
    db.query_row(
        "SELECT sqlite_ivm_create(?1, ?2)",
        [
            "probe_view",
            "SELECT group_id AS g, COUNT(*) AS n, SUM(amount) AS s
             FROM items GROUP BY group_id",
        ],
        |r| r.get::<_, String>(0),
    )?;
    db.execute_batch("INSERT INTO items VALUES (1, 4, 7)")?;
    let view: (i64, i64) = db.query_row("SELECT n, s FROM probe_view WHERE g = 4", [], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    assert_eq!(view, (1, 7));
    db.execute_batch("INSERT INTO items VALUES (2, 4, 3)")?;
    let view: (i64, i64) = db.query_row("SELECT n, s FROM probe_view WHERE g = 4", [], |r| {
        Ok((r.get(0)?, r.get(1)?)
        )
    })?;
    assert_eq!(view, (2, 10));
    Ok(())
}

#[test]
fn loaded_extension_retains_preparation_and_execution_events() {
    // A separate process lets the actual dylib install its hafley-observe layer.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "loaded_extension_maintains_a_view_end_to_end", "--nocapture"])
        .env("IVM_EXTENSION", artifact())
        .env("RUST_LOG", "sqlite_ivm=trace,sqlite=trace")
        .env("HAFLEY_LOG_FORMAT", "json")
        .env("HAFLEY_TRACE", "")
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let records = String::from_utf8(output.stderr).unwrap().lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let count = |message: &str| records.iter().filter(|r| r["fields"]["message"] == message).count();
    assert!(count("prepare_start") > 0);
    assert_eq!(count("prepare_start"), count("prepare_end"));
    assert_eq!(count("execute_start"), count("execute_end"));
    assert!(count("loop_iteration") > 0);
    for record in records.iter().filter(|r| r["fields"]["message"] == "prepare_start") {
        let span = &record["span"];
        assert_eq!(span["name"], "prepare");
        let sql = span["sql"].as_str().expect("SQL must precede preparation");
        assert!(!sql.is_empty());
        assert_eq!(span["sql_bytes"].as_u64(), Some(sql.len() as u64));
    }
    for name in ["attach", "populate", "populate_node", "insert", "sync", "prepare", "execute"] {
        assert!(records.iter().any(|r| r["fields"]["message"] == "new" && r["span"]["name"] == name), "missing native entry {name}");
    }
}
