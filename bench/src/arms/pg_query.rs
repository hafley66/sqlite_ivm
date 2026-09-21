//! pg-query arm: same cluster and DDL, plain recompute SELECT per state.

use super::pg_ivm::{pg_projection, pg_rows, pg_write_rows, Cluster, PgGroup};
use super::{verify_inputs, verify_output, Arm, Measure, Setup};
use crate::fixture::{Fixture, State};
use anyhow::Result;
use postgres::Client;
use std::sync::Arc;
use std::time::Instant;
pub struct PgQuery {
    cluster: Arc<Cluster>,
    query: String,
    columns: usize,
    db: Option<Client>,
    group: Option<PgGroup>,
}

impl PgQuery {
    pub fn new(cluster: Arc<Cluster>) -> Self {
        Self { cluster, query: String::new(), columns: 0, db: None, group: None }
    }
}

impl Arm for PgQuery {
    fn name(&self) -> &'static str {
        "pg-query"
    }

    fn setup(&mut self, fixture: &Fixture) -> Result<Setup> {
        let mut db = self.cluster.client(self.cluster.dbname)?;
        super::pg_ivm::create_tables(&mut db)?;
        self.query = fixture.query.clone();
        self.group = Some(PgGroup::new(self.cluster.postmaster_pid()?)?);
        self.columns = fixture.column_count();
        self.db = Some(db);
        Ok(Setup::Ready)
    }

    fn apply(&mut self, state: &State) -> Result<Measure> {
        let db = self.db.as_mut().expect("setup before apply");
        let started = Instant::now();
        db.batch_execute(&format!("BEGIN;{} COMMIT;", state.mutation_sql))?;
        let wall = started.elapsed();
        let actual = pg_rows(db.query(&pg_projection(self.columns, &self.query), &[])?)?;
        let checksum = verify_output(state, actual, None)?;
        verify_inputs(state, |table| pg_write_rows(db, table))?;
        Ok(Measure { wall, checksum })
    }

    fn teardown(&mut self) -> Result<()> {
        if self.db.is_some() {
            self.group.as_mut().expect("group sampler").sample();
        }
        self.db = None;
        Ok(())
    }
}
