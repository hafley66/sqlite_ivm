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
