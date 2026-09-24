use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

fn extension_path() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = root.join("ext/Cargo.toml");
    let target = root.join("ext/target");
    let built = Command::new("cargo")
        .args(["build", "--offline", "--manifest-path"])
        .arg(manifest)
        .args(["--target-dir"])
        .arg(&target)
        .output()
        .expect("build DD extension");
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let suffix = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    target
        .join("debug")
        .join(format!("libfrontier_dd_ext.{suffix}"))
}

fn rows(db: &Connection, sql: &str) -> rusqlite::Result<Vec<Vec<i64>>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let result = statement
        .query_map([], |row| {
            (0..width)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}

#[test]
fn extension_settles_and_recovers_from_generation_gap() -> rusqlite::Result<()> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(
        "CREATE TABLE membership(id INTEGER PRIMARY KEY,person INTEGER,team INTEGER);
         CREATE TABLE permission(id INTEGER PRIMARY KEY,team INTEGER,resource INTEGER);
         CREATE TABLE direct_grant(id INTEGER PRIMARY KEY,person INTEGER,resource INTEGER);
         CREATE TABLE job(id INTEGER PRIMARY KEY,team INTEGER,cost INTEGER);",
    )?;
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(extension_path(), Some("sqlite3_frontier_dd_ext_init"))?;
        db.load_extension_disable()?;
    }
    db.query_row("SELECT dd_frontier_install('access')", [], |_| Ok(()))?;
    db.query_row("SELECT dd_frontier_install('team_cost')", [], |_| Ok(()))?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES(1,1,10);
         INSERT INTO permission VALUES(1,10,100);
         INSERT INTO direct_grant VALUES(1,1,100);
         INSERT INTO job VALUES(1,10,5),(2,10,7);
         COMMIT;",
    )?;
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_access")?,
        vec![vec![1, 100]]
    );
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_team_cost")?,
        vec![vec![10, 2, 12]]
    );

    // The catalog rolls back with SQLite, while the in-process DD worker may
    // already have advanced in xSync. Force that observable generation gap.
    db.execute("UPDATE dd_frontier_catalog SET generation=0", [])?;
    db.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES(2,2,10);
         INSERT INTO permission VALUES(2,10,200);
         UPDATE job SET cost=8 WHERE id=2;
         COMMIT;",
    )?;
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_access ORDER BY 1,2")?,
        vec![vec![1, 100], vec![1, 200], vec![2, 100], vec![2, 200]]
    );
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_team_cost")?,
        vec![vec![10, 2, 13]]
    );
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_access_delta ORDER BY 2,3")?,
        vec![vec![1, 1, 200], vec![1, 2, 100], vec![1, 2, 200]]
    );

    db.execute_batch("BEGIN; INSERT INTO direct_grant VALUES(3,9,900); ROLLBACK;")?;
    assert_eq!(
        rows(&db, "SELECT * FROM dd_frontier_access ORDER BY 1,2")?,
        vec![vec![1, 100], vec![1, 200], vec![2, 100], vec![2, 200]]
    );
    db.query_row("SELECT dd_frontier_drop('access')", [], |_| Ok(()))?;
    db.query_row("SELECT dd_frontier_drop('team_cost')", [], |_| Ok(()))?;
    let remaining: i64 = db.query_row("SELECT count(*) FROM dd_frontier_catalog", [], |row| {
        row.get(0)
    })?;
    assert_eq!(remaining, 0);
    Ok(())
}
