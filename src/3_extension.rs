use crate::query::{error, quote};
use hafley_observe::{Config, FormatConfig, OutputFormat};
use rusqlite::{functions::FunctionFlags, Connection, Result};
use std::io::IsTerminal;
use tracing_subscriber::{
    fmt::{format::FmtSpan, writer::BoxMakeWriter},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

static OBSERVE: std::sync::Once = std::sync::Once::new();

// Maintenance logs no events, so spans report on close with their busy/idle time.
// A host process that already owns a global subscriber keeps it; try_init fails quietly.
fn observe() {
    OBSERVE.call_once(|| {
        let ansi = std::io::stderr().is_terminal();
        let version = env!("CARGO_PKG_VERSION");
        let config = Config::from_env("sqlite_ivm", version, "warn", ansi).unwrap_or(Config {
            service_name: "sqlite_ivm",
            service_version: version,
            default_filter: "warn",
            format: OutputFormat::Human,
            ansi,
        });
        let format = FormatConfig {
            span_events: FmtSpan::CLOSE,
            ..FormatConfig::standard(config.format, config.ansi)
        };
        let installed = tracing_subscriber::registry()
            .with(hafley_observe::env_filter(config.default_filter))
            .with(hafley_observe::format_layer(
                format,
                BoxMakeWriter::new(std::io::stderr),
            ))
            .try_init();
        if installed.is_ok() {
            hafley_observe::startup(&config);
        }
    });
}

pub fn register(db: &Connection) -> Result<()> {
    observe();
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
        c"sqlite_ivm_real_hex",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value: rusqlite::types::Value = ctx.get(0)?;
            match value {
                rusqlite::types::Value::Real(v) => Ok(Some(format!("{:016x}", v.to_bits()))),
                _ => Ok(None),
            }
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
