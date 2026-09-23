use rusqlite::{Connection, Result};
use std::{path::Path, process::Command};

fn extension() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/0_native/Cargo.toml");
    let built = Command::new("cargo")
        .args(["build", "--offline", "--quiet", "--manifest-path"])
        .arg(manifest)
        .output()
        .expect("build native fixture");
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "fixtures/0_native/target/debug/libengine_iso_native_fixture.{suffix}"
    ))
}

#[test]
fn loaded_extension_maintains_the_join_union() -> Result<()> {
    let db = Connection::open_in_memory()?;
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(extension(), Some("sqlite3_engine_iso_init"))?;
        db.load_extension_disable()?;
    }
    assert_eq!(
        db.query_row("SELECT engine_iso_access_install()", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    db.execute_batch(
        "BEGIN; INSERT INTO src_0 VALUES(1,1,10); INSERT INTO src_1 VALUES(1,10,100); COMMIT;",
    )?;
    let first: Vec<(i64, i64, i64)> = db
        .prepare("SELECT person,resource,weight FROM __iso_support ORDER BY person,resource")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_>>()?;
    assert_eq!(first, [(1, 100, 1)]);
    db.execute_batch(
        "BEGIN; INSERT INTO src_0 VALUES(2,2,10); INSERT INTO src_1 VALUES(2,10,200); COMMIT;",
    )?;
    let after: Vec<(i64, i64)> = db
        .prepare("SELECT person,resource FROM __iso_support ORDER BY person,resource")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_>>()?;
    assert_eq!(after, [(1, 100), (1, 200), (2, 100), (2, 200)]);
    Ok(())
}

#[test]
fn loaded_extension_maintains_group_count_sum() -> Result<()> {
    let db = Connection::open_in_memory()?;
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(extension(), Some("sqlite3_engine_iso_init"))?;
        db.load_extension_disable()?;
    }
    assert_eq!(
        db.query_row("SELECT engine_iso_group_install()", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    db.execute_batch("BEGIN; INSERT INTO src_0 VALUES(1,10,5),(2,10,7); COMMIT;")?;
    let first: (i64, i64, i64) = db.query_row(
        "SELECT group_id,jobs,total_cost FROM __iso_groups",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(first, (10, 2, 12));
    db.execute_batch("UPDATE src_0 SET c1=-7 WHERE id=2;")?;
    let second: (i64, i64, i64) = db.query_row(
        "SELECT group_id,jobs,total_cost FROM __iso_groups",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(second, (10, 2, -2));
    Ok(())
}
