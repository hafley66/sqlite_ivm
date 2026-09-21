use crate::{
    catalog::error,
    census::{self, Phase, CACHED},
    relational::{Kind, Plan},
    relational_maintenance::{BULK_MULTIPLICITY_BUDGET, BULK_ROUND_BUDGET, Row},
    relational_program::{
        ArrangementStatements, FixpointSide, FixpointStatements, KindStatements, Program,
        SplitStatements, UpsertStatements, CLEAR_TOUCHED,
    },
};
use rusqlite::{params, params_from_iter, types::Value, Connection, Result};

/// Bounds the per-row seed loop: one prepared insert bound per staged source
/// row. The collector already caps staging; this is the census-side ceiling.
const CENSUS_SEED_BUDGET: usize = 1_000_000;

impl Plan {
    /// The temp scratch every drain writes: one out table and one before table
    /// per node, plus the touched-key set. Runs at bind time, outside any
    /// trigger program, because DDL inside a trigger aborts the statement.
    pub(crate) fn prepare_scratch(&self, db: &Connection, program: &Program) -> Result<()> {
        for sql in &program.scratch {
            census::batch(db, Phase::Declare, &program.name, sql)?;
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
            // One span per node seed; each bound execution fires its own trace event.
            let statement = census::open(Phase::Drain, name, insert, CACHED);
            let _statement = statement.enter();
            let mut insert = db.prepare_cached(insert.as_str())?;
            let mut bound = 0usize;
            for (source, row, d) in batch {
                if *source != input || *d == 0 {
                    continue;
                }
                if bound >= CENSUS_SEED_BUDGET {
                    return Err(error("seed statement budget exceeded"));
                }
                bound += 1;
                insert.execute(params_from_iter(row.iter().chain(std::iter::once(&Value::Integer(*d)))))?;
            }
            statement.rows(bound);
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
                    census::query_cached(
                        db,
                        Phase::Drain,
                        name,
                        &statements.touched[side],
                        [],
                        |r| r.get::<_, bool>(0),
                    )
                })
                .collect::<Result<Vec<bool>>>()?;
            if !touched_inputs.iter().any(|t| *t) {
                continue;
            }
            let _node = tracing::debug_span!("node", kind = node.kind.label(), id).entered();
            match &statements.kind {
                KindStatements::Input { .. } => {}
                KindStatements::Map { materialize } => materialize.execute(db, name)?,
                KindStatements::SetAll { copies } => {
                    for sql in copies {
                        census::exec_cached(db, Phase::Maintain, name, sql, [])?;
                    }
                }
                KindStatements::Arrangement(arrangement) => {
                    self.drain_arrangement(db, name, arrangement, &touched_inputs)?
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
            census::exec_cached(db, Phase::Drain, name, &statements.sweep, [])?;
        }
        Ok(())
    }
    fn drain_arrangement(
        &self,
        db: &Connection,
        name: &str,
        statements: &ArrangementStatements,
        touched_inputs: &[bool],
    ) -> Result<()> {
        // Touched keys: every key of every input delta row, interned.
        census::exec_cached(db, Phase::Maintain, name, CLEAR_TOUCHED, [])?;
        for side in &statements.sides {
            let side = side
                .as_ref()
                .ok_or_else(|| error("arrangement node without a key"))?;
            census::exec_cached(db, Phase::Maintain, name, &side.intern, [])?;
            census::exec_cached(db, Phase::Maintain, name, &side.touch, [])?;
        }
        // Before: the node's output over the touched keys, parked.
        statements.materialize.execute(db, name)?;
        census::exec_cached(db, Phase::Maintain, name, &statements.park, [])?;
        census::exec_cached(db, Phase::Maintain, name, &statements.clear_out, [])?;
        for (side, touched) in touched_inputs.iter().enumerate() {
            if *touched {
                let side = statements.sides[side]
                    .as_ref()
                    .ok_or_else(|| error("arrangement node without a key"))?;
                upsert(db, name, Phase::Maintain, &side.upsert)?;
            }
        }
        // After, then out = after minus before as a bag.
        statements.materialize.execute(db, name)?;
        for sql in &statements.diff {
            census::exec_cached(db, Phase::Maintain, name, sql, [])?;
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
                census::exec_cached(db, Phase::Fixpoint, name, &statements_side.intern, [])?;
                census::exec_cached(db, Phase::Fixpoint, name, &statements_side.touch, [])?;
                split_side(db, name, &statements_side.split)?;
                upsert(db, name, Phase::Fixpoint, &statements_side.upsert)?;
                self.fixpoint(db, name, id, statements, statements_side)?;
            }
        }
        Ok(())
    }
    /// Applies the output node's delta to the result rows: retractions delete
    /// that many copies by key, additions insert that many copies.
    fn apply_state(&self, db: &Connection, program: &Program) -> Result<()> {
        let name = program.name.as_str();
        let statements = &program.apply_state;
        let wanted: i64 = census::query_cached(db, Phase::Maintain, name, &statements.wanted, [], |r| {
            r.get(0)
        })?;
        let removed = census::exec_cached(db, Phase::Maintain, name, &statements.retract, [])?;
        if removed as i64 != wanted {
            return Err(error(format!(
                "missing result multiplicity: {wanted} retractions, {removed} rows present"
            )));
        }
        let peak: i64 = census::query_cached(db, Phase::Maintain, name, &statements.peak, [], |r| {
            r.get(0)
        })?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        census::exec_cached(db, Phase::Maintain, name, &statements.extend, [])?;
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
            census::query_cached(db, Phase::Fixpoint, name, sql, [], |r| r.get(0))
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
                    written += census::exec_cached(
                        db,
                        Phase::Fixpoint,
                        name,
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
        let any_left: bool = census::query_cached(db, Phase::Fixpoint, name, &side.exists_left, [], |r| {
            r.get(0)
        })?;
        if any_left {
            census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_work, [])?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_deleted, [])?;
            for sqls in &side.left_derives {
                for sql in sqls {
                    census::exec_cached(db, Phase::Fixpoint, name, sql, [])?;
                }
            }
            let mut lo = 0;
            let mut delete_rounds = 0usize;
            loop {
                if delete_rounds >= BULK_ROUND_BUDGET {
                    return Err(error("fixpoint closure round budget exceeded"));
                }
                delete_rounds += 1;
                let hi = max_rowid(&statements.max_work)?;
                if hi == lo {
                    break;
                }
                let round = round_span("delete");
                census::exec_cached(db, Phase::Fixpoint, name, &statements.collect_deleted, params![lo, hi])?;
                census::exec_cached(db, Phase::Fixpoint, name, &statements.drop_deleted_range, params![lo, hi])?;
                let mut written = 0;
                for sql in &statements.delete_derives {
                    written += census::exec_cached(
                        db,
                        Phase::Fixpoint,
                        name,
                        sql,
                        params_from_iter([Value::Integer(lo), Value::Integer(hi)]),
                    )?;
                }
                round.record("rows", written);
                lo = hi;
            }
            let restored = max_rowid(&statements.max_all)?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.restore, [])?;
            rounds(restored)?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_work, [])?;
        }
        // Arrivals: derive forward from the new rows, then close.
        for sqls in &side.arrive_derives {
            for sql in sqls {
                census::exec_cached(db, Phase::Fixpoint, name, sql, [])?;
            }
        }
        rounds(lo)?;
        // Deltas: a deleted member gone for good retracts; one stored again with
        // another representative retracts the old row and emits the new one;
        // rows past `lo` that were not deleted this pass are new.
        if any_left {
            census::exec_cached(db, Phase::Fixpoint, name, &statements.retract_gone, [])?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.emit_stored, [])?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.emit_fresh, [lo])?;
            census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_deleted, [])?;
        } else {
            census::exec_cached(db, Phase::Fixpoint, name, &statements.seed_new, [lo])?;
        }
        Ok(())
    }
}

/// Adds one side's delta rows (in that input's out table) to the
/// arrangement. Existing identities take the summed multiplicity; new
/// identities are inserted; zero rows leave; a negative row is an error.
fn upsert(db: &Connection, name: &str, phase: Phase, statements: &UpsertStatements) -> Result<()> {
    census::exec_cached(db, phase, name, &statements.clear_delta, [])?;
    census::exec_cached(db, phase, name, &statements.fill_delta, [])?;
    census::exec_cached(db, phase, name, &statements.apply, [])?;
    census::exec_cached(db, phase, name, &statements.insert_new, [])?;
    let bad: bool = census::query_cached(db, phase, name, &statements.bad, [], |r| r.get(0))?;
    if bad {
        return Err(error("negative arrangement multiplicity"));
    }
    census::exec_cached(db, phase, name, &statements.drop_zero, [])?;
    Ok(())
}

/// Splits one side's delta into the rows entering its arrangement and the
/// rows leaving it, by net multiplicity against what is stored.
fn split_side(db: &Connection, name: &str, statements: &SplitStatements) -> Result<()> {
    census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_arrived, [])?;
    census::exec_cached(db, Phase::Fixpoint, name, &statements.clear_left, [])?;
    census::exec_cached(db, Phase::Fixpoint, name, &statements.fill_left, [])?;
    census::exec_cached(db, Phase::Fixpoint, name, &statements.fill_arrived, [])?;
    Ok(())
}
