use crate::query::{error, quote};
use rusqlite::{functions::FunctionFlags, Connection, Result};

pub fn register(db: &Connection) -> Result<()> {
    crate::vtab::register(db)?;
    crate::source_ddl::register(db)?;
    db.create_scalar_function(
        c"sqlite_ivm_create",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let name: String = ctx.get(0)?;
            let sql: String = ctx.get(1)?;
            // Borrow the invoking connection for this callback; SQLite retains ownership.
            let db = unsafe { ctx.get_connection()? };
            let query = sql.replace('\'', "''");
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE main.{} USING sqlite_ivm('{query}')",
                quote(&name)
            ))?;
            Ok(name)
        },
    )?;
    db.create_scalar_function(
        c"sqlite_ivm_drop",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let name: String = ctx.get(0)?;
            let db = unsafe { ctx.get_connection()? };
            let canonical: String = db
                .query_row(
                    "SELECT name FROM main.__ivm_views WHERE name=?1",
                    [&name],
                    |r| r.get(0),
                )
                .map_err(|_| error("no managed view with that name"))?;
            db.execute_batch(&format!("DROP TABLE main.{}", quote(&canonical)))?;
            Ok(canonical)
        },
    )
}

#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sqlite3_extension_init(
    db: *mut rusqlite::ffi::sqlite3,
    error: *mut *mut std::os::raw::c_char,
    api: *mut rusqlite::ffi::sqlite3_api_routines,
) -> std::os::raw::c_int {
    // The library initializes the SQLite API table and owns ABI error conversion.
    unsafe {
        Connection::extension_init2(db, error, api, |db| {
            register(&db)?;
            Ok(false)
        })
    }
}
