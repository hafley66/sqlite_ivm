//! sqlite-ivm arm: extension load, persistent virtual table, per-state loop,
//! and the untimed reopen/rename/rollback/drop lifecycle checks from
//! `examples/4_sqlite_case.rs`.

use super::{
    read_rows, read_write_rows, remove_db_files, source_ddl, sqlite_pragmas, verify_inputs,
    verify_output, Arm, Measure, Setup,
};
use crate::fixture::{Fixture, State};
use anyhow::{bail, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct SqliteIvm {
    path: PathBuf,
    extension: PathBuf,
    query: String,
    db: Option<Connection>,
    disk: Option<u64>,
}

impl SqliteIvm {
    pub fn new(path: PathBuf, extension: PathBuf) -> Self {
        Self { path, extension, query: String::new(), db: None, disk: None }
    }

    /// Extension dylib: `IVM_EXTENSION` override, else next to the bench
    /// binary (the target directory the root `--features extension` build
    /// writes into under the shared CARGO_TARGET_DIR).
    pub fn resolve_extension() -> Result<PathBuf> {
        if let Ok(path) = std::env::var("IVM_EXTENSION") {
            return Ok(PathBuf::from(path));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for name in ["libsqlite_ivm.dylib", "libsqlite_ivm.so"] {
                    let candidate = dir.join(name);
                    if candidate.exists() {
                        return Ok(candidate);
                    }
                }
            }
        }
        bail!(
            "sqlite_ivm extension dylib not found; run `cargo build --release \
             --features extension` at the repo root or set IVM_EXTENSION"
        )
    }
}

fn load_extension(db: &Connection, path: &Path) -> Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("extension path is not UTF-8"))?;
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(path, None::<&str>)?;
        db.load_extension_disable()?;
    }
    Ok(())
}

impl Arm for SqliteIvm {
    fn name(&self) -> &'static str {
        "sqlite-ivm"
    }

    fn setup(&mut self, fixture: &Fixture) -> Result<Setup> {
        remove_db_files(&self.path);
        let db = Connection::open(&self.path)?;
        db.execute_batch(sqlite_pragmas())?;
        db.execute_batch(source_ddl())?;
        load_extension(&db, &self.extension)?;
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE circuit_view USING sqlite_ivm('{}')",
            fixture.query.replace('\'', "''")
        ))?;
        self.query = fixture.query.clone();
        self.db = Some(db);
        Ok(Setup::Ready)
    }

    fn apply(&mut self, state: &State) -> Result<Measure> {
        let db = self.db.as_ref().expect("setup before apply");
        let started = Instant::now();
        db.execute_batch(&format!("BEGIN;{} COMMIT;", state.mutation_sql))?;
        let actual = read_rows(db, "SELECT * FROM circuit_view")?;
        let wall = started.elapsed();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let mut disk = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if let Ok(wal) = std::fs::metadata(format!("{}-wal", self.path.display())) {
            disk += wal.len();
        }
        self.disk = Some(disk);
        let oracle = read_rows(db, &self.query)?;
        let checksum = verify_output(state, actual, Some(oracle))?;
        verify_inputs(state, |table| read_write_rows(db, table))?;
        Ok(Measure { wall, checksum })
    }

    fn teardown(&mut self) -> Result<()> {
        let db = match self.db.take() {
            Some(db) => db,
            None => return Ok(()),
        };
        // Untimed persistence checks, outside the measurement interval.
        let expected = read_rows(&db, "SELECT * FROM circuit_view")?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.disk = Some(std::fs::metadata(&self.path)?.len());
        drop(db);
        let reopened = Connection::open(&self.path)?;
        load_extension(&reopened, &self.extension)?;
        reopened.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;")?;
        if read_rows(&reopened, "SELECT * FROM circuit_view")? != expected {
            bail!("reopen mismatch");
        }
        reopened.execute_batch(
            "ALTER TABLE circuit_view RENAME TO reopened_view;BEGIN;DELETE FROM a;ROLLBACK;",
        )?;
        if read_rows(&reopened, "SELECT * FROM reopened_view")? != expected {
            bail!("rename/rollback mismatch");
        }
        reopened.execute_batch("DROP TABLE reopened_view")?;
        drop(reopened);
        remove_db_files(&self.path);
        Ok(())
    }

    fn peak_rss_bytes(&self) -> Option<u64> {
        super::peak_rss_bytes()
    }

    fn disk_bytes(&self) -> Option<u64> {
        self.disk
    }
}
