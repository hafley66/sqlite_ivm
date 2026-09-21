// No file-level gate: under a bare `cargo test` the three tests exist and run,
// skipping with a named reason when in-process loading is unavailable. That
// availability is structural on this platform, not a preference: the system
// libsqlite3 exports no sqlite3_load_extension symbols (link error:
// _sqlite3_enable_load_extension, _sqlite3_load_extension), refuses
// sqlite3_auto_extension with SQLITE_MISUSE even for a no-op entry point, and
// ships a CLI with .load removed; and defaulting `rusqlite/load_extension`
// breaks the `extension` build itself (E0425: sqlite3_enable_load_extension is
// absent from the loadable-extension bindings, rusqlite inner_connection.rs).
// The tier that can load in-process is bench plus rusqlite/bundled, which
// scripts/1_crud.sh drives through the shared helper. A loadable-extension
// rlib never opens connections in a test process, so no test here can run
// under `--features extension`; with bench off the skip keeps that build safe.
// No memory budget: a loaded extension resolves its own allocator symbols, so
// a process counting allocator would report a number that means nothing.

use rusqlite::{Connection, Result};
use std::path::PathBuf;

/// scripts/0_build.sh writes the artifact here; IVM_NATIVE_EXTENSION overrides it.
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

/// None means the case is skipped, with a named reason; the artifact-absent
/// skip is what keeps a bare `cargo test` honest before scripts/0_build.sh.
fn open_loaded() -> Result<Option<Connection>> {
    let path = artifact();
    if !path.exists() {
        eprintln!(
            "6_extension_load: skipping, no artifact at {}; run scripts/0_build.sh",
            path.display()
        );
        return Ok(None);
    }
    #[cfg(not(feature = "bench"))]
    {
        let _ = &path;
        eprintln!(
            "6_extension_load: skipping, in-process extension loading needs the bench tier; \
             run: cargo test --locked --features bench --features rusqlite/bundled \
             --test 6_extension_load"
        );
        Ok(None)
    }
    #[cfg(feature = "bench")]
    {
        // The named entry point makes a rename a hard failure instead of a
        // fallback to the filename-derived symbol.
        let db = Connection::open_in_memory()?;
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(&path, Some("sqlite3_extension_init"))?;
        }
        Ok(Some(db))
    }
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
    let Some(db) = open_loaded()? else {
        return Ok(());
    };
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
