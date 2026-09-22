use crate::{
    catalog::error,
    statements::{self, Phase},
    relational::{Kind, Plan},
    relational_maintenance::{BULK_MULTIPLICITY_BUDGET, BULK_ROUND_BUDGET, Row},
    relational_program::{
        ArrangementStatements, FixpointSide, FixpointStatements, KindStatements, Program,
        SplitStatements, CLEAR_TOUCHED,
    },
};
use rusqlite::{params, params_from_iter, types::Value, Connection, Result};

/// Bounds the per-row seed loop: one prepared insert bound per staged source
/// row. The collector already caps staging; this is the statements-side ceiling.
const SEED_STATEMENT_BUDGET: usize = 1_000_000;

impl Plan {
    /// The temp scratch every drain writes: one out table and one before table
    /// per node, plus the touched-key set. Runs at bind time, outside any
    /// trigger program, because DDL inside a trigger aborts the statement.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn prepare_scratch(&self, db: &Connection, program: &Program) -> Result<()> {
        for sql in &program.scratch {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            statements::batch(db, Phase::Declare, &program.name, sql)?;
        }
        hafley_observe::sqlite_memory::record_memory(db, "scratch_ready");
        Ok(())
    }
    /// Set-at-a-time maintenance of one batch. Every node kind runs a constant
    /// number of statements per batch; row counts live inside SQLite. Node ids
    /// are topological because `push` appends after its inputs.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn drain(&self, db: &Connection, program: &Program, batch: &[(usize, Row, i64)]) -> Result<()> {
        let name = program.name.as_str();
        let _span = tracing::debug_span!("drain", view = name, rows = batch.len()).entered();
        let mut out_rows = vec![0usize; self.nodes.len()];
        let seed = tracing::debug_span!("node", kind = "seed", id = self.nodes.len()).entered();
        // One prepared insert per input node; the batch is the only per-row loop.
        for (id, node) in self.nodes.iter().enumerate() {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            let Kind::Input(input) = node.kind else {
                continue;
            };
            let KindStatements::Input { seed: insert } = &program.node(self, db, id).kind else {
                unreachable!()
            };
            // One span per bound execution, so each seed insert carries its own
            // site, prepared flag and changes() count.
            let mut bound = 0usize;
            for (source, row, d) in batch {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                if *source != input || *d == 0 {
                    continue;
                }
                if bound >= SEED_STATEMENT_BUDGET {
                    return Err(error("seed statement budget exceeded"));
                }
                bound += 1;
                out_rows[id] += statements::exec_cached(
                    db,
                    Phase::Drain,
                    name,
                    insert,
                    params_from_iter(row.iter().chain(std::iter::once(&Value::Integer(*d)))),
                )?;
            }
        }
        drop(seed);
        for id in 0..self.nodes.len() {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            let node = &self.nodes[id];
            if matches!(node.kind, Kind::Input(_)) {
                continue;
            }
            let touched_inputs = node
                .inputs
                .iter()
                .map(|input| out_rows[*input] != 0)
                .collect::<Vec<_>>();
            if !touched_inputs.iter().any(|t| *t) {
                tracing::trace!(id, kind = node.kind.label(), "node_skipped_unchanged_inputs");
                continue;
            }
            let statements = program.node(self, db, id);
            let _node = tracing::debug_span!("node", kind = node.kind.label(), id).entered();
            let written = match &statements.kind {
                KindStatements::Input { .. } => unreachable!(),
                KindStatements::Map { materialize } => materialize.execute(db, name)?,
                KindStatements::SetAll { copies } => {
                    let mut written = 0usize;
                    for sql in copies {
                        tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                        written += statements::exec_cached(db, Phase::Maintain, name, sql, [])?;
                    }
                    written
                }
                KindStatements::SetMembership { sides, statements: state, fallback, consolidation } => {
                    statements::exec_cached(db,Phase::Maintain,name,CLEAR_TOUCHED,[])?;
                    for side in sides {
                        let side = side.as_ref().ok_or_else(||error("set without a key"))?;
                        statements::exec_cached(db,Phase::Maintain,name,&side.intern,[])?;
                        statements::exec_cached(db,Phase::Maintain,name,&side.touch,[])?;
                    }
                    statements::exec_cached(db,Phase::Maintain,name,&state.before,[])?;
                    statements::exec_cached(db,Phase::Maintain,name,&state.update,[])?;
                    self.check_set_membership(db,name,&state.bad)?;
                    let recalc: bool = statements::query_cached(db,Phase::Maintain,name,&state.representative_removed,[],|r|r.get(0))?;
                    if recalc {
                        fallback.execute(db,name)?;
                        statements::exec_cached(db,Phase::Maintain,name,&state.replace_representatives,[])?;
                        statements::exec_cached(db,Phase::Maintain,name,&statements.sweep,[])?;
                    }
                    statements::exec_cached(db,Phase::Maintain,name,&state.remove_empty,[])?;
                    statements::exec_cached(db,Phase::Maintain,name,&state.after,[])?;
                    for sql in &consolidation[..4] {
                        statements::exec_cached(db,Phase::Maintain,name,sql,[])?;
                    }
                    let written = statements::exec_cached(db,Phase::Maintain,name,&consolidation[4],[])?;
                    statements::exec_cached(db,Phase::Maintain,name,&state.clear_before,[])?;
                    written
                }
                KindStatements::Arrangement(arrangement) => {
                    self.drain_arrangement(db, name, arrangement, &touched_inputs)?
                }
                KindStatements::Fixpoint(fixpoint) => {
                    let Kind::Fixpoint { .. } = &node.kind else {
                        unreachable!()
                    };
                    self.drain_fixpoint(db, name, id, fixpoint, &touched_inputs)?
                }
            };
            tracing::debug!(id, written, inputs = ?touched_inputs, "node_delta_emitted");
            out_rows[id] = written;
            if id == self.output && written != 0 {
                let _apply = tracing::debug_span!("node", kind = "apply_state", id).entered();
                self.apply_state(db, program)?;
            }
            for (side, child) in node.inputs.iter().enumerate() {
                if self.out_last_reads[*child] == id
                    && out_rows[*child] != 0
                    && !node.inputs[..side].contains(child)
                {
                    statements::exec_cached(
                        db,
                        Phase::Drain,
                        name,
                        &program.node(self, db, *child).sweep,
                        [],
                    )?;
                }
            }
            if self.out_last_reads[id] == id && written != 0 {
                statements::exec_cached(db, Phase::Drain, name, &statements.sweep, [])?;
            }
        }
        // Long-lived output and aggregate inputs remain until after apply_state.
        // A failed drain aborts the enclosing statement and its temp writes.
        let _sweep = tracing::debug_span!("node", kind = "sweep", id = self.nodes.len()).entered();
        for (id, rows) in out_rows.into_iter().enumerate() {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            if rows != 0 && self.out_last_reads[id] == self.nodes.len() {
                statements::exec_cached(db, Phase::Drain, name, &program.node(self, db, id).sweep, [])?;
            }
        }
        tracing::info!(compiled_sql_nodes = program.nodes.iter().filter(|node| node.get().is_some()).count(), total_nodes = self.nodes.len(), "maintenance_sql_cache");
        hafley_observe::sqlite_memory::record_memory(db, "drain_complete");
        Ok(())
    }
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    fn drain_arrangement(
        &self,
        db: &Connection,
        name: &str,
        statements: &ArrangementStatements,
        touched_inputs: &[bool],
    ) -> Result<usize> {
        if let Some(join) = &statements.join_delta {
            let mut written = 0;
            for (side, touched) in touched_inputs.iter().enumerate() {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                if !touched {
                    continue;
                }
                written += statements::exec_cached(db, Phase::Maintain, name, &join.sides[side], [])?;

            }
            if touched_inputs.iter().all(|t| *t) {
                written += statements::exec_cached(db, Phase::Maintain, name, &join.cross, [])?;
            }
            let bad: bool = statements::query_cached(db, Phase::Maintain, name, &join.bad, [], |r| r.get(0))?;
            if bad {
                return Err(error("join multiplicity overflow"));
            }
            if !join.consolidate {
                return Ok(written);
            }
        } else {
            // Touched keys: every key of every input delta row, interned.
            statements::exec_cached(db, Phase::Maintain, name, CLEAR_TOUCHED, [])?;
            for side in &statements.sides {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                let side = side
                    .as_ref()
                    .ok_or_else(|| error("arrangement node without a key"))?;
                statements::exec_cached(db, Phase::Maintain, name, &side.intern, [])?;
                statements::exec_cached(db, Phase::Maintain, name, &side.touch, [])?;
            }
            // Before: the node's output over the touched keys, parked.
            if let Some(stored_before) = &statements.stored_before {
                let corrupt: bool = statements::query_cached(db, Phase::Maintain, name, &stored_before.corrupt, [], |row| row.get(0))?;
                if corrupt {
                    statements.materialize_before.execute(db, name)?;
                } else {
                    statements::exec_cached(db, Phase::Maintain, name, &stored_before.read, [])?;
                }
            } else {
                statements.materialize_before.execute(db, name)?;
            }
            statements::exec_cached(db, Phase::Maintain, name, &statements.park, [])?;
            statements::exec_cached(db, Phase::Maintain, name, &statements.clear_out, [])?;
            // After, then out = after minus before as a bag.
            if let Some(aggregate) = &statements.aggregate_delta {
                let eligible: bool = statements::query_cached(db, Phase::Maintain, name, &aggregate.eligible, [], |row| row.get(0))?;
                if eligible {
                    let _aggregate = tracing::debug_span!("aggregate_delta", path = "integer").entered();
                    statements::exec_cached(db, Phase::Maintain, name, &aggregate.update_counts, [])?;
                    statements::exec_cached(db, Phase::Maintain, name, &aggregate.apply, [])?;
                } else {
                    let _aggregate = tracing::debug_span!("aggregate_delta", path = "recompute").entered();
                    statements::exec_cached(db, Phase::Maintain, name, &aggregate.invalidate, [])?;
                    statements.materialize.execute(db, name)?;
                }
            } else {
                statements.materialize.execute(db, name)?;
            }
            for sql in &statements.diff {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                statements::exec_cached(db, Phase::Maintain, name, sql, [])?;
            }
        }
        statements::exec_cached(db, Phase::Maintain, name, &statements.consolidate, [])?;
        statements::exec_cached(db, Phase::Maintain, name, &statements.clear_out, [])?;
        let written = statements::exec_cached(db, Phase::Maintain, name, &statements.emit, [])?;
        statements::exec_cached(db, Phase::Maintain, name, &statements.clear_before, [])?;
        Ok(written)
    }
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    fn drain_fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        statements: &FixpointStatements,
        touched_inputs: &[bool],
    ) -> Result<usize> {
        let mut written = 0usize;
        for (side, touched) in touched_inputs.iter().enumerate() {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            if *touched {
                let statements_side = statements.sides[side]
                    .as_ref()
                    .ok_or_else(|| error("arrangement node without a key"))?;
                split_side(db, name, &statements_side.split)?;

                written += self.fixpoint(db, name, id, statements, statements_side)?;
            }
        }
        statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_work, [])?;
        Ok(written)
    }
    /// Applies the output node's delta to the result rows: retractions delete
    /// that many copies by key, additions insert that many copies.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    fn apply_state(&self, db: &Connection, program: &Program) -> Result<()> {
        let name = program.name.as_str();
        let statements = &program.apply_state;
        let wanted: i64 = statements::query_cached(db, Phase::Maintain, name, &statements.wanted, [], |r| {
            r.get(0)
        })?;
        let replaced = match &statements.replace {
            Some(sql) => statements::exec_cached(db,Phase::Maintain,name,sql,[])?,
            None => 0,
        };
        let removed = replaced + statements::exec_cached(db, Phase::Maintain, name, &statements.retract, [])?;
        if removed as i64 != wanted {
            return Err(error(format!(
                "missing result multiplicity: {wanted} retractions, {removed} rows present"
            )));
        }
        let peak: i64 = statements::query_cached(db, Phase::Maintain, name, &statements.peak, [], |r| {
            r.get(0)
        })?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        statements::exec_cached(db, Phase::Maintain, name, &statements.extend, [])?;
        Ok(())
    }
    /// Semi-naive closure over one side's arrived and left sets. Arrivals
    /// derive forward from the new rows; departures delete every member they
    /// reached, then rederive what survives another way. Deltas land in the
    /// node's out table.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    fn fixpoint(
        &self,
        db: &Connection,
        name: &str,
        id: usize,
        statements: &FixpointStatements,
        side: &FixpointSide,
    ) -> Result<usize> {
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
            statements::query_cached(db, Phase::Fixpoint, name, sql, [], |r| r.get(0))
        };
        let rounds = |mut lo: i64| -> Result<()> {
            let mut rounds = 0usize;
            loop {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
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
                    tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                    written += statements::exec_cached(
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
        let any_left: bool = statements::query_cached(db, Phase::Fixpoint, name, &side.exists_left, [], |r| {
            r.get(0)
        })?;
        if any_left {
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_work, [])?;
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_deleted, [])?;
            for sqls in &side.left_derives {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                for sql in sqls {
                    tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                    statements::exec_cached(db, Phase::Fixpoint, name, sql, [])?;
                }
            }
            let mut lo = 0;
            let mut delete_rounds = 0usize;
            loop {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                if delete_rounds >= BULK_ROUND_BUDGET {
                    return Err(error("fixpoint closure round budget exceeded"));
                }
                delete_rounds += 1;
                let hi = max_rowid(&statements.max_work)?;
                if hi == lo {
                    break;
                }
                let round = round_span("delete");
                statements::exec_cached(db, Phase::Fixpoint, name, &statements.collect_deleted, params![lo, hi])?;
                statements::exec_cached(db, Phase::Fixpoint, name, &statements.drop_deleted_range, params![lo, hi])?;
                let mut written = 0;
                for sql in &statements.delete_derives {
                    tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                    written += statements::exec_cached(
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
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.restore, [])?;
            rounds(restored)?;
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_work, [])?;
        }
        // Arrivals: derive forward from the new rows, then close.
        for sqls in &side.arrive_derives {
            tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
            for sql in sqls {
                tracing::trace!(source_file = file!(), source_line = line!(), "loop_iteration");
                statements::exec_cached(db, Phase::Fixpoint, name, sql, [])?;
            }
        }
        rounds(lo)?;
        // Deltas: a deleted member gone for good retracts; one stored again with
        // another representative retracts the old row and emits the new one;
        // rows past `lo` that were not deleted this pass are new.
        let written = if any_left {
            let mut written = 0usize;
            written += statements::exec_cached(db, Phase::Fixpoint, name, &statements.retract_gone, [])?;
            written += statements::exec_cached(db, Phase::Fixpoint, name, &statements.emit_stored, [])?;
            written += statements::exec_cached(db, Phase::Fixpoint, name, &statements.emit_fresh, [lo])?;
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_deleted, [])?;
            written
        } else {
            statements::exec_cached(db, Phase::Fixpoint, name, &statements.seed_new, [lo])?
        };
        Ok(written)
    }
}

/// Identify input identities whose support crosses zero in this batch.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
fn split_side(db: &Connection, name: &str, statements: &SplitStatements) -> Result<()> {
    statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_arrived, [])?;
    statements::exec_cached(db, Phase::Fixpoint, name, &statements.clear_left, [])?;
    statements::exec_cached(db, Phase::Fixpoint, name, &statements.fill_left, [])?;
    statements::exec_cached(db, Phase::Fixpoint, name, &statements.fill_arrived, [])?;
    Ok(())
}
