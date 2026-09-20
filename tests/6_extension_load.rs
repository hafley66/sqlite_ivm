#![cfg(all(feature = "bench", not(feature = "extension")))]
// Tests 0 through 5 call register(&db) and never reach sqlite3_extension_init;
// this file dlopens the shipped artifact so a renamed or dropped init symbol
// fails here instead of in the field.
// No memory budget: a dlopen'd library resolves its own allocator symbols, so
// a process counting allocator would report a number that means nothing.

use rusqlite::{Connection, LoadExtensionGuard, Result};
use std::path::PathBuf;

/// scripts/0_build.sh writes the artifact here; IVM_NATIVE_EXTENSION overrides it,
/// matching tests/support/0_database.rs.
fn artifact() -> PathBuf {
    if let Ok(path) = std::env::var("IVM_NATIVE_EXTENSION") {
        return PathBuf::from(path);
    }
    let name = if cfg!(target_os = "macos") {
        "libsqlite_ivm.dylib"
    } else if cfg!(target_os = "linux") {
        "libsqlite_ivm.so"
    } else {
        "sqlite_ivm.dll"
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug")
        .join(name)
}

/// None means the artifact is not built; each case reports that and skips
/// rather than reporting a false failure for a bare `cargo test`.
fn open_loaded() -> Result<Option<Connection>> {
    let path = artifact();
    if !path.exists() {
        eprintln!(
            "6_extension_load: skipping, no artifact at {}; run scripts/0_build.sh",
            path.display()
        );
        return Ok(None);
    }
    let db = Connection::open_in_memory()?;
    // sqlite_ivm_create refuses connections without these; set once for every case.
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON")?;
    unsafe {
        let _guard = LoadExtensionGuard::new(&db)?;
        // The entry point is named explicitly: the artifact contract is
        // sqlite3_extension_init, and a rename must fail the load instead of
        // falling back to the filename-derived symbol.
        db.load_extension(&path, Some("sqlite3_extension_init"))?;
    }
    Ok(Some(db))
}

#[test]
fn extension_loads_through_its_entry_point() -> Result<()> {
    let Some(_db) = open_loaded()? else {
        return Ok(());
    };
    Ok(())
}

#[test]
fn loaded_extension_registers_the_create_function() -> Result<()> {
    let Some(db) = open_loaded()? else {
        return Ok(());
    };
    db.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, amount INTEGER NOT NULL)",
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
    let Some(db) = open_loaded()? else {
        return Ok(());
    };
    db.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, amount INTEGER NOT NULL)",
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
        Ok((r.get(0)?, r.get(1)?))
    })?;
    assert_eq!(view, (2, 10));
    Ok(())
}
