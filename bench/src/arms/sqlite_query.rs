//! Plain SQLite arm: `examples/4_sqlite_case.rs` without the extension.

use super::{
    read_rows, read_write_rows, remove_db_files, source_ddl, sqlite_pragmas, verify_inputs,
    verify_output, Arm, Measure, Setup,
};
use crate::fixture::{Fixture, State};
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Instant;

pub struct SqliteQuery {
    path: PathBuf,
    query: String,
    db: Option<Connection>,
}

impl SqliteQuery {
    pub fn new(path: PathBuf) -> Self {
        Self { path, query: String::new(), db: None }
    }
}

impl Arm for SqliteQuery {
    fn name(&self) -> &'static str {
        "sqlite-query"
    }

    fn setup(&mut self, fixture: &Fixture) -> Result<Setup> {
        remove_db_files(&self.path);
        let db = Connection::open(&self.path)?;
        db.execute_batch(sqlite_pragmas())?;
        db.execute_batch(source_ddl())?;
        self.query = fixture.query.clone();
        self.db = Some(db);
        Ok(Setup::Ready)
    }

    fn apply(&mut self, state: &State) -> Result<Measure> {
        let db = self.db.as_ref().expect("setup before apply");
        let started = Instant::now();
        db.execute_batch(&format!("BEGIN;{} COMMIT;", state.mutation_sql))?;
        let actual = read_rows(db, &self.query)?;
        let wall = started.elapsed();
        let checksum = verify_output(state, actual, None)?;
        verify_inputs(state, |table| read_write_rows(db, table))?;
        Ok(Measure { wall, checksum })
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(db) = self.db.take() {
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            drop(db);
        }
        remove_db_files(&self.path);
        Ok(())
    }
}
