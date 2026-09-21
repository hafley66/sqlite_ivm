use crate::{
    catalog::error,
    relational::{Kind, Plan},
    relational_maintenance::{
        BULK_DEPARTURE_FRONTIER_MIN_ROWS, BULK_DEPARTURE_ROUND_BUDGET,
        BULK_DEPARTURE_SET_RESTORE_THRESHOLD, BULK_DEPARTURE_SET_ROUND_BUDGET,
        BULK_MULTIPLICITY_BUDGET, BULK_ROUND_BUDGET, CachedExecute, Row,
    },
    relational_program::{
        ArrangementStatements, FixpointSide, FixpointStatements, KindStatements, Program,
        SplitStatements, UpsertStatements, CLEAR_TOUCHED,
    },
};
use rusqlite::{params_from_iter, types::Value, Connection, Result};

impl Plan {
    /// The temp scratch every drain writes: one out table and one before table
    /// per node, plus the touched-key set. Runs at bind time, outside any
    /// trigger program, because DDL inside a trigger aborts the statement.
    pub(crate) fn prepare_scratch(&self, db: &Connection, program: &Program) -> Result<()> {
        for sql in &program.scratch {
            db.execute_batch(sql)?;
        }
        Ok(())
    }
    /// Set-at-a-time maintenance of one batch. Every node kind runs a constant
    /// number of statements per batch; row counts live inside SQLite. Node ids
    /// are topological because `push` appends after its inputs.
    pub(crate) fn drain(&self, db: &Connection, program: &Program, batch: &[(usize, Row, i64)]) -> Result<()> {
        let name = program.name.as_str();
        let _span = tracing::debug_span!("drain", view = name, rows = batch.len()).entered();
        let seed = tracing::debug_span!("node", kind = "seed", id = self.nodes.len()).entered();
        // One prepared insert per input node; the batch is the only per-row loop.
        for (id, node) in self.nodes.iter().enumerate() {
            let Kind::Input(input) = node.kind else {
                continue;
            };
            let KindStatements::Input { seed: insert } = &program.nodes[id].kind else {
                unreachable!()
            };
            let mut insert = db.prepare_cached(insert.as_str())?;
            for (source, row, d) in batch {
                if *source != input || *d == 0 {
                    continue;
                }
                insert.execute(params_from_iter(row.iter().chain(std::iter::once(&Value::Integer(*d)))))?;
            }
        }
        drop(seed);
        for id in 0..self.nodes.len() {
            let node = &self.nodes[id];
            let statements = &program.nodes[id];
            let touched_inputs = node
                .inputs
                .iter()
                .enumerate()
                .map(|(side, _)| {
                    db.prepare_cached(&statements.touched[side])?
                        .query_row([], |r| r.get::<_, bool>(0))
                })
                .collect::<Result<Vec<bool>>>()?;
            if !touched_inputs.iter().any(|t| *t) {
                continue;
            }
            let _node = tracing::debug_span!("node", kind = node.kind.label(), id).entered();
            match &statements.kind {
                KindStatements::Input { .. } => {}
                KindStatements::Map { materialize } => materialize.execute(db)?,
                KindStatements::SetAll { copies } => {
                    for sql in copies {
                        db.execute_cached(sql, [])?;
                    }
                }
                KindStatements::Arrangement(arrangement) => {
                    self.drain_arrangement(db, arrangement, &touched_inputs)?
                }
                KindStatements::Fixpoint(fixpoint) => {
                    let Kind::Fixpoint { .. } = &node.kind else {
                        unreachable!()
                    };
                    self.drain_fixpoint(db, name, id, fixpoint, &touched_inputs)?;
                }
            }
            if id == self.output {
                let _apply = tracing::debug_span!("node", kind = "apply_state", id).entered();
                self.apply_state(db, program)?;
            }
        }
        // Sweep is the only clear: seed trusts it, and a failed drain aborts the
        // enclosing statement, which unwinds the temp writes with it.
        let _sweep = tracing::debug_span!("node", kind = "sweep", id = self.nodes.len()).entered();
        for statements in &program.nodes {
            db.execute_cached(&statements.sweep, [])?;
        }
        Ok(())
    }
    fn drain_arrangement(
        &self,
        db: &Connection,
        statements: &ArrangementStatements,
        touched_inputs: &[bool],
    ) -> Result<()> {
        // Touched keys: every key of every input delta row, interned.
        db.execute_cached(CLEAR_TOUCHED, [])?;
        for side in &statements.sides {
            let side = side
                .as_ref()
                .ok_or_else(|| error("arrangement node without a key"))?;
            db.execute_cached(&side.intern, [])?;
            db.execute_cached(&side.touch, [])?;
        }
        // Before: the node's output over the touched keys, parked.
        statements.materialize.execute(db)?;
        db.execute_cached(&statements.park, [])?;
        db.execute_cached(&statements.clear_out, [])?;
        for (side, touched) in touched_inputs.iter().enumerate() {
            if *touched {
                let side = statements.sides[side]
                    .as_ref()
                    .ok_or_else(|| error("arrangement node without a key"))?;
                upsert(db, &side.upsert)?;
            }
        }
        // After, then out = after minus before as a bag.
        statements.materialize.execute(db)?;
        for sql in &statements.diff {
            db.execute_cached(sql, [])?;
        }
        Ok(())
    }
    fn drain_fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        statements: &FixpointStatements,
        touched_inputs: &[bool],
    ) -> Result<()> {
        for (side, touched) in touched_inputs.iter().enumerate() {
            if *touched {
                let statements_side = statements.sides[side]
                    .as_ref()
                    .ok_or_else(|| error("arrangement node without a key"))?;
                db.execute_cached(&statements_side.intern, [])?;
                db.execute_cached(&statements_side.touch, [])?;
                split_side(db, &statements_side.split)?;
                upsert(db, &statements_side.upsert)?;
                self.fixpoint(db, name, id, statements, statements_side)?;
            }
        }
        Ok(())
    }
    /// Applies the output node's delta to the result rows: retractions delete
    /// that many copies by key, additions insert that many copies.
    fn apply_state(&self, db: &Connection, program: &Program) -> Result<()> {
        let statements = &program.apply_state;
        let wanted: i64 = db
            .prepare_cached(&statements.wanted)?
            .query_row([], |r| r.get(0))?;
        let removed = db.execute_cached(&statements.retract, [])?;
        if removed as i64 != wanted {
            return Err(error(format!(
                "missing result multiplicity: {wanted} retractions, {removed} rows present"
            )));
        }
        let peak: i64 = db
            .prepare_cached(&statements.peak)?
            .query_row([], |r| r.get(0))?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        db.execute_cached(&statements.extend, [])?;
        Ok(())
    }
    /// Semi-naive closure over one side's arrived and left sets. Arrivals
    /// derive forward from the new rows; departures delete every member they
    /// reached, then rederive what survives another way. Deltas land in the
    /// node's out table.
    fn fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        statements: &FixpointStatements,
        side: &FixpointSide,
    ) -> Result<()> {
        let span = tracing::debug_span!(
            "fixpoint",
            view = name,
            node = id,
            rounds = tracing::field::Empty
        )
        .entered();
        let round_count = std::cell::Cell::new(0usize);
        let round_span = |phase: &'static str| {
            let index = round_count.get();
            round_count.set(index + 1);
            span.record("rounds", index + 1);
            tracing::debug_span!("round", phase, index, rows = tracing::field::Empty).entered()
        };
        let max_rowid = |sql: &str| -> Result<i64> {
            db.prepare_cached(sql)?.query_row([], |r| r.get(0))
        };
        let rounds = |mut lo: i64| -> Result<()> {
            let mut rounds = 0usize;
            loop {
                if rounds >= BULK_ROUND_BUDGET {
                    return Err(error("fixpoint closure round budget exceeded"));
                }
                rounds += 1;
                let hi = max_rowid(&statements.max_all)?;
                if hi == lo {
                    return Ok(());
                }
                let round = round_span("derive");
                let mut written = 0;
                for sql in &statements.round_derives {
                    written += db.execute_cached(
                        sql,
                        params_from_iter([Value::Integer(lo), Value::Integer(hi)]),
                    )?;
                }
                round.record("rows", written);
                lo = hi;
            }
        };
        // Every member past `lo` at the end is new unless the delete pass stored
        // it first; rederived rows take fresh rowids, so `lo` precedes that pass.
        let lo = max_rowid(&statements.max_all)?;
        // Departures first: delete everything the left rows reached, rederive.
        let any_left: bool = db
            .prepare_cached(&side.exists_left)?
            .query_row([], |r| r.get(0))?;
        if any_left {
            db.execute_cached(&statements.clear_work, [])?;
            db.execute_cached(&statements.clear_deleted, [])?;
            let mut work_rows = 0usize;
            for sqls in &side.left_derives {
                for sql in sqls {
                    work_rows += db.execute_cached(sql, [])?;
                }
            }
            db.execute_cached(&statements.clear_departing, [])?;
            db.execute_cached(&statements.seed_departing, [])?;
            let mut settled = false;
            let mut frontier = 0usize;
            if work_rows >= BULK_DEPARTURE_FRONTIER_MIN_ROWS {
                for generation in 0..BULK_DEPARTURE_SET_ROUND_BUDGET {
                    let round = round_span("delete");
                    db.execute_cached(&statements.collect_departing[generation], [])?;
                    db.execute_cached(&statements.drop_departing[generation], [])?;
                    let mut written = 0;
                    for sql in &statements.delete_derives[generation] {
                        written += db.execute_cached(sql, [])?;
                    }
                    round.record("rows", written);
                    if written == 0 {
                        settled = true;
                        break;
                    }
                    frontier = generation + 1;
                }
            }
            if !settled {
                db.execute_cached(&statements.clear_work, [])?;
                db.execute_cached(&statements.seed_work_deleted, [])?;
                db.execute_cached(&statements.seed_work_frontier[frontier], [])?;
                let mut delete_rounds = 0usize;
                loop {
                    if delete_rounds >= BULK_DEPARTURE_ROUND_BUDGET {
                        return Err(error("fixpoint recursive departure round budget exceeded"));
                    }
                    delete_rounds += 1;
                    let round = round_span("delete_recursive");
                    db.execute_cached(&statements.collect_deleted, [])?;
                    db.execute_cached(&statements.drop_deleted, [])?;
                    let mut written = 0;
                    for sql in &statements.recursive_delete_derives {
                        written += db.execute_cached(sql, [])?;
                    }
                    round.record("rows", written);
                    if written == 0 {
                        break;
                    }
                }
            }
            let restored = max_rowid(&statements.max_all)?;
            let deleted_rows: i64 = db
                .prepare_cached(&statements.deleted_rows)?
                .query_row([], |r| r.get(0))?;
            if deleted_rows < BULK_DEPARTURE_SET_RESTORE_THRESHOLD as i64 {
                db.execute_cached(&statements.restore_small, [])?;
            } else {
                for sql in &statements.restores {
                    db.execute_cached(sql, [])?;
                }
            }
            rounds(restored)?;
            db.execute_cached(&statements.clear_work, [])?;
        }
        // Arrivals: derive forward from the new rows, then close.
        for sqls in &side.arrive_derives {
            for sql in sqls {
                db.execute_cached(sql, [])?;
            }
        }
        rounds(lo)?;
        // Deltas: a deleted member gone for good retracts; one stored again with
        // another representative retracts the old row and emits the new one;
        // rows past `lo` that were not deleted this pass are new.
        if any_left {
            db.execute_cached(&statements.retract_gone, [])?;
            db.execute_cached(&statements.emit_stored, [])?;
            db.execute_cached(&statements.emit_fresh, [lo])?;
            db.execute_cached(&statements.clear_deleted, [])?;
        } else {
            db.execute_cached(&statements.seed_new, [lo])?;
        }
        Ok(())
    }
}

/// Adds one side's delta rows (in that input's out table) to the
/// arrangement. Existing identities take the summed multiplicity; new
/// identities are inserted; zero rows leave; a negative row is an error.
fn upsert(db: &Connection, statements: &UpsertStatements) -> Result<()> {
    db.execute_cached(&statements.clear_delta, [])?;
    db.execute_cached(&statements.fill_delta, [])?;
    db.execute_cached(&statements.apply, [])?;
    db.execute_cached(&statements.insert_new, [])?;
    let bad: bool = db
        .prepare_cached(&statements.bad)?
        .query_row([], |r| r.get(0))?;
    if bad {
        return Err(error("negative arrangement multiplicity"));
    }
    db.execute_cached(&statements.drop_zero, [])?;
    Ok(())
}

/// Splits one side's delta into the rows entering its arrangement and the
/// rows leaving it, by net multiplicity against what is stored.
fn split_side(db: &Connection, statements: &SplitStatements) -> Result<()> {
    db.execute_cached(&statements.clear_arrived, [])?;
    db.execute_cached(&statements.clear_left, [])?;
    db.execute_cached(&statements.fill_left, [])?;
    db.execute_cached(&statements.fill_arrived, [])?;
    Ok(())
}
