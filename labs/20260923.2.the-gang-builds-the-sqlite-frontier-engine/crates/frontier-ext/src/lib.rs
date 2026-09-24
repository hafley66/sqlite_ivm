//! `frontier_ext` — a SQLite loadable extension exposing the incremental
//! frontier engine as two scalar functions:
//!
//! - `frontier_install(name, select_sql)` installs a program (its settle
//!   objects, its watch collector, its catalog rows) and returns 1.
//! - `frontier_drop(name)` tears a program down and returns 1.
//!
//! Both are SQLITE_DIRECTONLY: they may not run inside triggers or views,
//! because they create and drop schema objects. The engine's own collector
//! (wired by [`Program::install`]) drives settlement at COMMIT, so a settle
//! failure fails the enclosing COMMIT and a rollback leaves no trace.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use frontier_engine::{Frontier, Program};
use sqlite_ext::Plugin;
use sqlite_ext::rusqlite::{functions::FunctionFlags, Connection, Error, Result};

/// Programs installed through this extension instance, keyed by name. The
/// engine keeps its own watch collector alive inside the connection; this
/// registry only holds the handle needed for `frontier_drop`.
static REGISTRY: LazyLock<Mutex<HashMap<String, Program>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn install(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"frontier_install",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            let name = ctx.get_raw(0).as_str()?.to_string();
            let sql = ctx.get_raw(1).as_str()?;
            let program = Program::install(&db, &name, sql)?;
            REGISTRY.lock().expect("frontier registry").insert(name, program);
            Ok(1_i64)
        },
    )?;
    db.create_scalar_function(
        c"frontier_drop",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            let name = ctx.get_raw(0).as_str()?;
            let program = REGISTRY
                .lock()
                .expect("frontier registry")
                .remove(name)
                .ok_or_else(|| {
                    Error::ModuleError(format!("frontier_ext: no program named '{name}'"))
                })?;
            program.teardown(&db)?;
            Ok(1_i64)
        },
    )?;
    Ok(())
}

const PLUGIN: Plugin = Plugin::new("frontier_ext", env!("CARGO_PKG_VERSION"), "warn", install);

sqlite_ext::sqlite_extension!(sqlite3_frontier_ext_init, PLUGIN);
