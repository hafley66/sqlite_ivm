use crate::catalog::{error, quote};
use crate::statements::{self, Phase};
use frontier_engine::{Frontier, Program};
use rusqlite::{functions::FunctionFlags, Connection, Result};
const PLUGIN: sqlite_ext::Plugin =
    sqlite_ext::Plugin::new("sqlite_ivm", env!("CARGO_PKG_VERSION"), "warn", install);

#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub fn register(db: &Connection) -> Result<()> {
    PLUGIN.register(db)
}

fn install(db: &Connection) -> Result<()> {
    crate::vtab::register(db)?;
    crate::source_ddl::register(db)?;
    crate::relational_maintenance::register_functions(db)?;
    register_frontier(db)?;
    db.create_scalar_function(
        c"sqlite_ivm_create",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let _callback = tracing::trace_span!("sqlite_ivm_create").entered();
            let name: String = ctx.get(0)?;
            let sql: String = ctx.get(1)?;
            // Borrow the invoking connection for this callback; SQLite retains ownership.
            let db = unsafe { ctx.get_connection()? };
            let query = sql.replace('\'', "''");
            statements::batch(
                &db,
                Phase::Declare,
                &name,
                &format!(
                    "CREATE VIRTUAL TABLE main.{} USING sqlite_ivm('{query}')",
                    quote(&name)
                ),
            )?;
            Ok(name)
        },
    )?;
    db.create_scalar_function(
        c"sqlite_ivm_drop",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let _callback = tracing::trace_span!("sqlite_ivm_drop").entered();
            let name: String = ctx.get(0)?;
            let db = unsafe { ctx.get_connection()? };
            let canonical: String = statements::query(
                &db,
                Phase::Teardown,
                &name,
                "SELECT name FROM main.__ivm_views WHERE name=?1",
                [&name],
                |r| r.get(0),
            )
            .map_err(|_| error("no managed view with that name"))?;
            statements::batch(
                &db,
                Phase::Teardown,
                &canonical,
                &format!("DROP TABLE main.{}", quote(&canonical)),
            )?;
            Ok(canonical)
        },
    )
}

/// The frontier engine's own catalog and commit collector live on this SQLite
/// connection. No program handle is retained by the extension entry point.
fn register_frontier(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"sqlite_ivm_frontier_install",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let name: String = ctx.get(0)?;
            let select_sql: String = ctx.get(1)?;
            let db = unsafe { ctx.get_connection()? };
            Program::install(&db, &name, &select_sql)
                .map(|program| program.name().to_owned())
                .map_err(rusqlite::Error::from)
        },
    )?;
    db.create_scalar_function(
        c"sqlite_ivm_frontier_drop",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let name: String = ctx.get(0)?;
            let db = unsafe { ctx.get_connection()? };
            let program = Program::open(&db, &name).map_err(rusqlite::Error::from)?;
            program.teardown(&db).map_err(rusqlite::Error::from)?;
            Ok(name)
        },
    )
}

#[cfg(feature = "extension")]
sqlite_ext::sqlite_extension!(sqlite3_extension_init, PLUGIN);
