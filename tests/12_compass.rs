//! The golden compass, `tests/golden/compass.sql`, run two ways: in process
//! through rusqlite, and through the `sqlite3` CLI when `SQLITE3` names one
//! built with extension loading. The SQL file owns setup, events, oracle and
//! checks; this file adds the two rails that need one statement per
//! arrangement width.
#[path = "support/0_database.rs"]
mod database;
use database::register;
use rusqlite::{Connection, Result};
use std::process::Command;

const COMPASS: &str = include_str!("golden/compass.sql");
/// Lines the CLI understands and rusqlite does not, plus the extension load
/// that `register` replaces in process.
fn in_process_sql() -> String {
    COMPASS
        .lines()
        .filter(|line| !line.starts_with('.') && !line.contains("load_extension("))
        .collect::<Vec<_>>()
        .join("\n")
}
fn arrangements(db: &Connection) -> Result<Vec<(String, usize)>> {
    db.prepare(
        "SELECT object_name FROM __ivm_objects WHERE view_name='compass' AND object_type='table' \
         AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__r')",
    )?
    .query_map([], |row| row.get::<_, String>(0))?
    .map(|name| {
        let name = name?;
        let width: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name GLOB 'c[0-9]*'",
            [&name],
            |row| row.get(0),
        )?;
        Ok((name, width as usize))
    })
    .collect()
}
fn checks(db: &Connection) -> Result<Vec<(String, i64)>> {
    db.prepare("SELECT name, ok FROM checks ORDER BY seq")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

#[test]
fn compass_passes_in_process() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(&in_process_sql())?;
    let results = checks(&db)?;
    let failed: Vec<&str> = results
        .iter()
        .filter(|(_, ok)| *ok == 0)
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(failed.is_empty(), "compass checks failed: {failed:?}");
    assert_eq!(results.len(), 9, "check count in compass.sql");

    // Source rows have no persistent per-operator duplicates.
    assert!(arrangements(&db)?.is_empty(), "compass retained copied input rows");
    let width = db.prepare("SELECT * FROM compass")?.column_count();
    let invalid: i64 = db.query_row(&format!("SELECT count(*) FROM compass_state WHERE __check!=sqlite_ivm_row_check({})",(0..width).map(|i|format!("c{i}")).collect::<Vec<_>>().join(",")),[],|r|r.get(0))?;
    assert_eq!(invalid,0,"stored output identity");

    // Rail: nothing waits between transactions.
    let waiting: i64 = db
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE 'compass%\\_delta' ESCAPE '\\'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|name| db.query_row(&format!("SELECT count(*) FROM \"{}\"", name?), [], |row| row.get::<_, i64>(0)))
        .sum::<Result<i64>>()?;
    assert_eq!(waiting, 0, "rows left in a delta table after commit");
    Ok(())
}

#[test]
fn compass_passes_through_the_cli() {
    let Ok(sqlite3) = std::env::var("SQLITE3") else {
        eprintln!("SQLITE3 unset; the CLI leg needs a sqlite3 built with extension loading");
        return;
    };
    let Ok(extension) = std::env::var("IVM_NATIVE_EXTENSION") else {
        eprintln!("IVM_NATIVE_EXTENSION unset; build with --features extension and point at the dylib");
        return;
    };
    let output = Command::new(sqlite3)
        .args([
            "-bail",
            ":memory:",
            &format!(".param set @extension '{}'", extension.replace('\'', "''")),
            &format!(".read {}/tests/golden/compass.sql", env!("CARGO_MANIFEST_DIR")),
        ])
        .output()
        .expect("sqlite3 runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "sqlite3 exit {:?}\n{stdout}\n{stderr}", output.status.code());
    assert!(stdout.lines().any(|line| line == "PASS 9 checks"), "no PASS line:\n{stdout}\n{stderr}");
}
