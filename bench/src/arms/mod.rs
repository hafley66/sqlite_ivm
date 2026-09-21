//! Arm trait, measurement record, and shared engine helpers.

pub mod dd;
pub mod pg_ivm;
pub mod pg_query;
pub mod sqlite_ivm;
pub mod sqlite_query;

use crate::fixture::{Inputs, State, WriteRow};
use crate::oracle::{digest, output_text, sort_rows, Cell};
use anyhow::{bail, Result};
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct Measure {
    pub wall: Duration,
    pub checksum: String,
}

pub enum Setup {
    Ready,
    /// pg_ivm rejected the view definition (SQLSTATE 0A000); report prints `-`.
    Unsupported { reason: String },
}

pub trait Arm {
    fn name(&self) -> &'static str;
    fn setup(&mut self, fixture: &crate::fixture::Fixture) -> Result<Setup>;
    fn apply(&mut self, state: &State) -> Result<Measure>;
    fn teardown(&mut self) -> Result<()>;
    /// Absolute peak RSS observed by the arm, bytes.
    fn peak_rss_bytes(&self) -> Option<u64> {
        None
    }
    /// Database bytes after checkpoint, where the arm is durable.
    fn disk_bytes(&self) -> Option<u64> {
        None
    }
}

/// Untimed verification shared by the SQL arms: output equals the fixture
/// expectation (and the SQL oracle when given); returns the canonical checksum.
pub fn verify_output(
    state: &State,
    actual: Vec<Vec<Cell>>,
    sql_oracle: Option<Vec<Vec<Cell>>>,
) -> Result<String> {
    let sorted = sort_rows(actual);
    if let Some(rows) = sql_oracle {
        if sort_rows(rows) != sorted {
            bail!("{}: SQL oracle mismatch", state.name);
        }
    }
    if sorted != state.expected.rows {
        bail!("{}: output mismatch", state.name);
    }
    let checksum = digest(&output_text(&sorted));
    if checksum != state.expected.checksum {
        bail!("{}: checksum mismatch", state.name);
    }
    Ok(checksum)
}

/// Untimed input verification: db rows per table equal the fixture inputs and
/// reproduce the fixture input hash.
pub fn verify_inputs(
    state: &State,
    mut read: impl FnMut(&str) -> Result<Vec<WriteRow>>,
) -> Result<()> {
    let mut tables = Inputs::default();
    for table in ["a", "b", "c"] {
        let mut actual = read(table)?;
        actual.sort_by_key(|x| x.0);
        let mut expected = match table {
            "a" => state.inputs.a.clone(),
            "b" => state.inputs.b.clone(),
            _ => state.inputs.c.clone(),
        };
        expected.sort_by_key(|x| x.0);
        if actual != expected {
            bail!("{}: source rows mismatch on {table}", state.name);
        }
        *tables.table_mut(table) = read(table)?;
    }
    let hash = digest(&crate::oracle::input_text(&tables.sorted()));
    if hash != state.input_hash {
        bail!("{}: input hash mismatch", state.name);
    }
    Ok(())
}

pub fn source_ddl() -> &'static str {
    "CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
     CREATE INDEX a_k ON a(k);CREATE INDEX a_v ON a(v);\
     CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
     CREATE INDEX b_k ON b(k);CREATE INDEX b_v ON b(v);\
     CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
     CREATE INDEX c_k ON c(k);CREATE INDEX c_v ON c(v);"
}

pub fn sqlite_pragmas() -> &'static str {
    "PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;"
}

pub fn remove_db_files(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let _ = std::fs::remove_file(&file);
    }
}

/// Peak RSS of this process, bytes, from the kernel high-water counter
/// (`/usr/bin/time -l` equivalent). macOS reports bytes, Linux KiB.
pub fn peak_rss_bytes() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let bytes = usage.ru_maxrss;
    #[cfg(not(target_os = "macos"))]
    let bytes = usage.ru_maxrss * 1024;
    Some(bytes as u64)
}
/// Cumulative disk I/O of this process, `(bytes read, bytes written)`.
/// macOS reads `ri_diskio_bytesread`/`ri_diskio_byteswritten` from
/// `proc_pid_rusage`; Linux reads `/proc/self/io`. `None` elsewhere.
pub fn disk_io_bytes() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V2,
                &mut info as *mut libc::rusage_info_v2 as *mut libc::rusage_info_t,
            )
        };
        if rc != 0 {
            return None;
        }
        Some((info.ri_diskio_bytesread, info.ri_diskio_byteswritten))
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/self/io").ok()?;
        let field = |name: &str| -> Option<u64> {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|value| value.trim().parse().ok())
        };
        Some((field("read_bytes:")?, field("write_bytes:")?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

pub(crate) fn cell_from_sqlite(value: rusqlite::types::Value) -> Result<Cell> {
    Ok(match value {
        rusqlite::types::Value::Integer(n) => Cell::Int(n),
        rusqlite::types::Value::Text(s) => Cell::Text(s),
        other => bail!("unexpected SQLite value {other:?}"),
    })
}

pub(crate) fn read_rows(db: &rusqlite::Connection, sql: &str) -> Result<Vec<Vec<Cell>>> {
    let mut statement = db.prepare(sql)?;
    let columns = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            (0..columns)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|row| row.into_iter().map(cell_from_sqlite).collect())
        .collect()
}

pub(crate) fn read_write_rows(
    db: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<WriteRow>> {
    let mut statement = db.prepare(&format!("SELECT id,k,v FROM {table}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(WriteRow(row.get(0)?, Cell::Int(row.get(1)?), Cell::Int(row.get(2)?)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Run one external program (initdb/pg_ctl only), logging output to `log`.
pub fn run_logged(log: &Path, program: &Path, args: &[&std::ffi::OsStr]) -> Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .stderr(std::process::Stdio::from(std::fs::File::create(log)?))
        .stdout(std::process::Stdio::from(
            std::fs::File::create(log.with_extension("out"))?,
        ))
        .output()?;
    if !output.status.success() {
        bail!(
            "{} {:?} failed with {}; see {}",
            program.display(),
            args,
            output.status,
            log.display()
        );
    }
    Ok(())
}
