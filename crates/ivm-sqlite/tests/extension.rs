//! Gate: the native extension loads into a plain host connection and both
//! oracle sequences run end-to-end through pure SQL.
//!
//! Builds the `frontier-ext` cdylib in its own workspace (rusqlite
//! `loadable_extension` cannot share a build graph with this test's
//! `bundled` rusqlite), loads it with `sqlite3_frontier_ext_init`, then
//! drives `frontier_install` / `frontier_drop` and both packet cases.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Builds the cdylib and returns its path.
fn extension_path() -> PathBuf {
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../labs/20260923.2.the-gang-builds-the-sqlite-frontier-engine/crates/frontier-ext",
    );
    let target = PathBuf::from(std::env::var("CARGO_TARGET_DIR").expect("CARGO_TARGET_DIR for isolated extension build"))
        .join("frontier-ext");
    let built = Command::new("cargo")
        .args(["build", "--offline", "-j", "4", "--manifest-path"])
        .arg(ext.join("Cargo.toml"))
        // A fixed target dir pins the artifact path even when the ambient
        // CARGO_TARGET_DIR points elsewhere.
        .arg("--target-dir")
        .arg(&target)
        .output()
        .expect("cargo builds the frontier-ext cdylib");
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
    target.join("debug")
        .join(format!("libfrontier_ext.{suffix}"))
}

fn load(path: &PathBuf) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
         CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
         CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
         CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);",
    )
    .unwrap();
    unsafe {
        db.load_extension_enable().unwrap();
        db.load_extension(path, Some("sqlite3_frontier_ext_init"))
            .unwrap();
        db.load_extension_disable().unwrap();
    }
    db
}

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn pairs(db: &Connection, view: &str) -> Vec<(i64, i64)> {
    let mut stmt = db
        .prepare(&format!(
            "SELECT person, resource FROM {view} ORDER BY person, resource"
        ))
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn frontier_id(db: &Connection, name: &str) -> i64 {
    db.query_row(
        "SELECT frontier FROM frontier_catalog WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn extension_loads_and_settles_both_oracle_sequences() {
    let path = extension_path();
    let db = load(&path);

    // Install through pure SQL.
    assert_eq!(
        scalar(&db, "SELECT frontier_install('access', 'SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team')"),
        1
    );

    // The settled view answers; the catalog tracks the frontier counter.
    assert!(pairs(&db, "frontier_access").is_empty());
    assert_eq!(frontier_id(&db, "access"), 0);

    db.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1,10),(1,20);
         INSERT INTO permission VALUES (10,100),(20,100);
         INSERT INTO direct_grant VALUES (3,300);
         COMMIT;",
    )
    .unwrap();
    assert_eq!(pairs(&db, "frontier_access"), vec![(1, 100), (3, 300)]);
    let mut stmt = db
        .prepare("SELECT __sign, person, resource FROM frontier_access_delta ORDER BY person")
        .unwrap();
    let delta = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(delta, vec![(1, 1, 100), (1, 3, 300)]);

    // Frontier 2..5: cross-term, duplicate union support, retraction.
    db.execute_batch("BEGIN; INSERT INTO membership VALUES (2,10); INSERT INTO permission VALUES (10,200); COMMIT;").unwrap();
    assert_eq!(
        pairs(&db, "frontier_access"),
        vec![(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)]
    );
    db.execute_batch("INSERT INTO direct_grant VALUES (1,200);")
        .unwrap();
    assert_eq!(
        pairs(&db, "frontier_access"),
        vec![(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)]
    );
    db.execute_batch("DELETE FROM permission WHERE team=20 AND resource=100;")
        .unwrap();
    assert_eq!(
        pairs(&db, "frontier_access"),
        vec![(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)]
    );
    assert_eq!(frontier_id(&db, "access"), 4);

    // The aggregate case through the same installed extension.
    assert_eq!(
        scalar(&db, "SELECT frontier_install('team_cost', 'SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team')"),
        1
    );
    db.execute_batch("BEGIN; INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11); COMMIT;")
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT jobs, total_cost FROM frontier_team_cost WHERE team = 10",
            [],
            |row| { Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)) }
        )
        .unwrap(),
        (2, 12)
    );

    // A settle-time failure inside COMMIT: overflowing sums abort the commit.
    // SQLite rolls the whole transaction back on its own, so the previous
    // committed state (and only it) stays readable.
    let failed = db.execute_batch(
        "BEGIN;
         INSERT INTO job VALUES (4,10,9223372036854775807);
         INSERT INTO job VALUES (5,10,9223372036854775806);
         COMMIT;",
    );
    assert!(failed.is_err(), "commit with overflowing settle must fail");
    assert!(
        db.is_autocommit(),
        "a failed COMMIT leaves no open transaction"
    );
    assert_eq!(
        db.query_row(
            "SELECT jobs, total_cost FROM frontier_team_cost WHERE team = 10",
            [],
            |row| { Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)) }
        )
        .unwrap(),
        (2, 12)
    );
    let still_open = db.query_row("SELECT count(*) FROM job WHERE id IN (4,5)", [], |row| {
        row.get::<_, i64>(0)
    });
    assert_eq!(still_open, Ok(0));

    // Dropping both programs removes every frontier object and keeps sources.
    assert_eq!(scalar(&db, "SELECT frontier_drop('access')"), 1);
    assert_eq!(scalar(&db, "SELECT frontier_drop('team_cost')"), 1);
    // Every program object goes; only the shared catalog tables remain.
    let leftovers: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name LIKE 'frontier_%' AND name NOT IN ('frontier_catalog','frontier_catalog_column')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
    let catalog_rows: i64 = db
        .query_row("SELECT count(*) FROM frontier_catalog", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(catalog_rows, 0);
    let sources: i64 = db
        .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('membership','permission','direct_grant','job')", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sources, 4);
}
