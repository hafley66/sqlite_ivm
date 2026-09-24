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
fn loaded_extension_runs_frontier_and_virtual_table_on_one_source() -> Result<()> {
    let db = open_loaded()?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
         CREATE TABLE job(id INTEGER PRIMARY KEY,team INTEGER NOT NULL,cost INTEGER NOT NULL);",
    )?;
    let installed: String = db.query_row(
        "SELECT sqlite_ivm_frontier_install(?1, ?2)",
        [
            "team_cost",
            "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team",
        ],
        |row| row.get(0),
    )?;
    assert_eq!(installed, "team_cost");
    db.execute_batch(
        "CREATE VIRTUAL TABLE old_path USING sqlite_ivm('SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team');
         BEGIN;
         INSERT INTO job VALUES(1,10,5),(2,10,7);
         COMMIT;",
    )?;
    let sql = "SELECT jobs,total_cost FROM {view} WHERE team=10";
    for view in ["frontier_team_cost", "old_path"] {
        let result: (i64, i64) = db.query_row(&sql.replace("{view}", view), [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        assert_eq!(result, (2, 12), "{view}");
    }
    db.execute_batch("DELETE FROM job WHERE id=1")?;
    for view in ["frontier_team_cost", "old_path"] {
        let result: (i64, i64) = db.query_row(&sql.replace("{view}", view), [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        assert_eq!(result, (1, 7), "{view}");
    }
    let dropped: String = db.query_row(
        "SELECT sqlite_ivm_frontier_drop('team_cost')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(dropped, "team_cost");
    db.execute_batch("INSERT INTO job VALUES(3,10,3)")?;
    let legacy: (i64, i64) = db.query_row(
        "SELECT jobs,total_cost FROM old_path WHERE team=10",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(legacy, (2, 10));
    Ok(())
}

#[test]
fn loaded_extension_composes_two_frontiers_on_commit() -> Result<()> {
    let db = open_loaded()?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
         CREATE TABLE body_a(person INTEGER PRIMARY KEY);
         CREATE TABLE body_b(person INTEGER PRIMARY KEY);
         CREATE TABLE grant_resource(person INTEGER PRIMARY KEY, resource INTEGER NOT NULL);",
    )?;
    let installed: String = db.query_row(
        "SELECT sqlite_ivm_frontier_compose(?1, ?2, ?3, ?4)",
        [
            "bodies",
            "SELECT person FROM body_a UNION SELECT person FROM body_b",
            "grants",
            "SELECT b.person, g.resource FROM frontier_bodies b JOIN grant_resource g ON g.person = b.person",
        ],
        |row| row.get(0),
    )?;
    assert_eq!(installed, "grants");
    db.execute_batch(
        "BEGIN;
         INSERT INTO body_a VALUES (1);
         INSERT INTO body_b VALUES (1);
         INSERT INTO grant_resource VALUES (1, 10), (2, 20);
         COMMIT;",
    )?;
    let rows = db
        .prepare("SELECT person, resource FROM frontier_grants ORDER BY person")?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(rows, [(1, 10)]);
    db.execute_batch("DELETE FROM body_a WHERE person = 1")?;
    let count: i64 = db.query_row("SELECT count(*) FROM frontier_grants", [], |row| row.get(0))?;
    assert_eq!(count, 1);
    db.execute_batch("DELETE FROM body_b WHERE person = 1")?;
    let count: i64 = db.query_row("SELECT count(*) FROM frontier_grants", [], |row| row.get(0))?;
    assert_eq!(count, 0);
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
