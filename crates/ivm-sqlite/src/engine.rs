//! Settle: one frontier of signed source changes to one net output delta.
//!
//! Read/write order per frontier, all inside the caller's transaction (the
//! extension path runs at the collector's `xSync`, before SQLite's pager
//! commit):
//!
//! 1. clear engine staging tables (stage, scan deltas, join deltas, touch, delta);
//! 2. stage the batch row-by-row, sequence numbered (`__seq` = batch position);
//! 3. per scan: net the staging into that scan's signed delta (`SUM(__sign)`
//!    grouped over the needed columns, zero nets dropped);
//! 4. per join: the three-term equation
//!    `W = dL ⋈ R + L ⋈ dR − dL ⋈ dR` over the live source tables and the two
//!    scan deltas — the cross-term appears exactly once;
//! 5. the root: capture before-images of touched rows, apply the summed
//!    weight/aggregate deltas, drop invisible rows, emit the net delta rows;
//! 6. bump the catalog frontier and read the delta back in output order.
//!
//! An error anywhere rolls back to the savepoint, leaving the previous
//! committed state readable.

use crate::catalog::{self, Installed};
use crate::error::{EngineError, ErrorKind, Stage};
use crate::meter::Meter;
use crate::observe;
use crate::{Cell, OutputChange, Sign, SourceChange, Tuple};
use ivm_engine::Counters;
use sqlite_ext::rusqlite::{self, Connection};
use std::sync::Arc;

/// Registers the transaction collector over the program's sources; committed
/// source writes then settle as one frontier at commit time.
pub(crate) fn watch_collector(conn: &Connection, inst: &Arc<Installed>) -> Result<(), EngineError> {
    let name = catalog::collector(&inst.name, inst.install);
    let sources: Vec<&str> = inst.plan.sources.iter().map(String::as_str).collect();
    sqlite_ext::watch(
        conn,
        &name,
        &sources,
        ProgramTrigger {
            program: Arc::clone(inst),
        },
    )
    .map_err(|e| EngineError::new(Stage::Install, &inst.name, ErrorKind::Sqlite(e.to_string())))
}

/// Reconnect a persisted standalone collector before this connection writes
/// any of its source tables. The collector and triggers remain in the file;
/// only their connection-local module and callback state are restored.
pub(crate) fn reattach_collector(
    conn: &Connection,
    inst: &Arc<Installed>,
) -> Result<(), EngineError> {
    let name = catalog::collector(&inst.name, inst.install);
    let sources: Vec<&str> = inst.plan.sources.iter().map(String::as_str).collect();
    sqlite_ext::reattach(
        conn,
        &name,
        &sources,
        ProgramTrigger {
            program: Arc::clone(inst),
        },
    )
    .map_err(|e| EngineError::new(Stage::Install, &inst.name, ErrorKind::Sqlite(e.to_string())))
}

pub(crate) struct ProgramTrigger {
    program: Arc<Installed>,
}

impl sqlite_ext::BulkTrigger for ProgramTrigger {
    fn on_batch(
        &mut self,
        db: &Connection,
        batch: &[sqlite_ext::RowChange],
    ) -> rusqlite::Result<()> {
        let mut changes: Vec<SourceChange> = Vec::with_capacity(batch.len());
        for change in batch {
            changes.push(SourceChange {
                relation: change.table.clone(),
                sign: match change.sign {
                    sqlite_ext::Sign::Insert => Sign::Insert,
                    sqlite_ext::Sign::Delete => Sign::Delete,
                },
                row: change.values.iter().map(catalog::cell_of).collect(),
            });
        }
        if changes.is_empty() {
            return Ok(());
        }
        settle(db, &self.program, &changes, true)
            .map(|_| ())
            .map_err(rusqlite::Error::from)
    }
}

fn fail(what: &str, object: &str, e: rusqlite::Error) -> EngineError {
    EngineError::new(
        Stage::Settle,
        object,
        ErrorKind::Sqlite(format!("{what}: {e}")),
    )
}

/// Settles one frontier. When `in_commit` is false the settle wraps itself in
/// a savepoint so a failure leaves the previous committed state intact; when
/// true it runs inside the committing transaction itself (the collector's
/// xSync), where SQLite forbids opening a savepoint and the failing COMMIT
/// already provides the atomicity.
pub(crate) fn settle(
    conn: &Connection,
    inst: &Installed,
    batch: &[SourceChange],
    in_commit: bool,
) -> Result<Vec<OutputChange>, EngineError> {
    settle_counted(conn, inst, batch, in_commit).map(|(changes, _)| changes)
}

pub(crate) fn settle_counted(
    conn: &Connection,
    inst: &Installed,
    batch: &[SourceChange],
    in_commit: bool,
) -> Result<(Vec<OutputChange>, Counters), EngineError> {
    let span = tracing::info_span!(
        target: observe::TARGET,
        observe::SETTLE_SPAN,
        program = %inst.name,
    );
    let _guard = span.enter();
    let mut counters = if matches!(inst.plan.root, crate::plan::Root::Nodes(_)) {
        Counters::measured()
    } else {
        Counters { statements: Some(0), ..Counters::default() }
    };
    counters.interned = None;
    if in_commit {
        let mut meter = Meter::default();
        let changes = settle_inner(conn, inst, batch, &mut meter, &mut counters)?;
        if inst.sql_text { counters.delta_rows.join = None; counters.rounds = None; }
        counters.rows_written = changes.len() as u64;
        *counters.statements.as_mut().unwrap() += meter.statements;
        return Ok((changes, counters));
    }
    let mut meter = Meter::default();
    conn.execute_batch("SAVEPOINT frontier_sp_settle;")
        .map_err(|e| fail("savepoint", &inst.name, e))?;
    *counters.statements.as_mut().unwrap() += 1;
    match settle_inner(conn, inst, batch, &mut meter, &mut counters) {
        Ok(changes) => {
            if inst.sql_text { counters.delta_rows.join = None; counters.rounds = None; }
            conn.execute_batch("RELEASE frontier_sp_settle;")
                .map_err(|e| fail("release", &inst.name, e))?;
            *counters.statements.as_mut().unwrap() += 1;
            counters.rows_written = changes.len() as u64;
            *counters.statements.as_mut().unwrap() += meter.statements;
            Ok((changes, counters))
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO frontier_sp_settle; RELEASE frontier_sp_settle;")
                .map_err(|e| fail("rollback", &inst.name, e))?;
            Err(e)
        }
    }
}

fn settle_inner(
    conn: &Connection,
    inst: &Installed,
    batch: &[SourceChange],
    meter: &mut Meter,
    counters: &mut Counters,
) -> Result<Vec<OutputChange>, EngineError> {
    let phase = "settle";
    validate_batch(inst, batch)?;

    for (object, sql) in &inst.sqls.clears {
        meter
            .exec(conn, phase, object, sql, [])
            .map_err(|e| fail("clear", object, e))?;
    }

    for (i, change) in batch.iter().enumerate() {
        let width = inst.plan.stage_width.max(1);
        let mut params = Vec::with_capacity(3 + width);
        params.push(Cell::Integer(i as i64));
        params.push(Cell::Text(change.relation.clone()));
        params.push(Cell::Integer(change.sign.as_integer()));
        params.extend(change.row.iter().cloned());
        params.resize(3 + width, Cell::Null);
        meter
            .exec(
                conn,
                phase,
                &change.relation,
                &inst.sqls.stage_insert,
                rusqlite::params_from_iter(params),
            )
            .map_err(|e| fail("stage", &change.relation, e))?;
    }
    if let crate::plan::Root::Nodes(nodes) = &inst.plan.root {
        if inst.sql_text { catalog::encode_stage(conn, inst)?; }
        let changes = nodes
            .run(conn, counters)
            .map_err(|e| fail("node settle", &inst.name, e))?;
        let cols = inst
            .output
            .iter()
            .map(|c| catalog::quote(&c.name))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO {}(__sign,{cols}) VALUES ({})",
            catalog::quote(catalog::delta(&inst.name)),
            std::iter::once("?1".to_owned()).chain(
                inst.ir.rel(inst.ir.outputs[0]).expect("output relation").cols.iter().enumerate()
                    .map(|(i, ty)| if inst.sql_text { catalog::decode_sql(*ty, &format!("?{}", i + 2)) } else { format!("?{}", i + 2) })
            ).collect::<Vec<_>>().join(",")
        );
        for (row, weight) in changes {
            let params = std::iter::once(weight)
                .chain(row.into_iter())
                .collect::<Vec<_>>();
            conn.execute(&sql, rusqlite::params_from_iter(params))
                .map_err(|e| fail("node delta", &inst.name, e))?;
        }
        meter
            .exec(
                conn,
                phase,
                catalog::catalog(),
                &inst.sqls.bump,
                [inst.name.as_str()],
            )
            .map_err(|e| fail("frontier bump", catalog::catalog(), e))?;
        return read_delta(conn, inst, meter);
    }

    unreachable!("runtime plans always use NodesPlan")
}

fn validate_batch(inst: &Installed, batch: &[SourceChange]) -> Result<(), EngineError> {
    for change in batch {
        let Some(scan) = inst.plan.scans.iter().find(|s| s.table == change.relation) else {
            return Err(EngineError::new(
                Stage::Collect,
                &change.relation,
                ErrorKind::UnknownRelation(change.relation.clone()),
            ));
        };
        if change.row.len() != scan.columns.len() {
            return Err(EngineError::new(
                Stage::Collect,
                &change.relation,
                ErrorKind::Arity {
                    relation: change.relation.clone(),
                    expected: scan.columns.len(),
                    got: change.row.len(),
                },
            ));
        }
    }
    Ok(())
}

fn read_delta(
    conn: &Connection,
    inst: &Installed,
    meter: &mut Meter,
) -> Result<Vec<OutputChange>, EngineError> {
    let rows: Vec<(i64, Vec<Cell>)> = meter
        .rows(
            conn,
            "read",
            &catalog::delta(&inst.name),
            &inst.sqls.read_delta,
            [],
            |row| {
                let sign = row.get::<_, i64>(0)?;
                let mut cells = Vec::with_capacity(inst.plan.output.len());
                for i in 1..=inst.plan.output.len() {
                    cells.push(catalog::cell_of(&row.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok((sign, cells))
            },
        )
        .map_err(|e| fail("delta read", &catalog::delta(&inst.name), e))?;
    Ok(rows
        .into_iter()
        .map(|(sign, row)| OutputChange {
            sign: if sign < 0 { Sign::Delete } else { Sign::Insert },
            row,
        })
        .collect())
}

pub(crate) fn snapshot(conn: &Connection, inst: &Installed) -> Result<Vec<Tuple>, EngineError> {
    let mut meter = Meter::default();
    let rows: Vec<Vec<Cell>> = meter
        .rows(
            conn,
            "read",
            &catalog::root(&inst.name),
            &inst.sqls.snapshot,
            [],
            |row| {
                let mut cells = Vec::with_capacity(inst.plan.output.len());
                for i in 0..inst.plan.output.len() {
                    cells.push(catalog::cell_of(&row.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(cells)
            },
        )
        .map_err(|e| {
            EngineError::new(
                Stage::Read,
                catalog::root(&inst.name),
                ErrorKind::Sqlite(e.to_string()),
            )
        })?;
    Ok(rows.into_iter().map(Tuple).collect())
}

pub(crate) fn frontier_id(conn: &Connection, inst: &Installed) -> Result<u64, EngineError> {
    let mut meter = Meter::default();
    meter
        .one(
            conn,
            "read",
            catalog::catalog(),
            &inst.sqls.read_frontier,
            [inst.name.as_str()],
            |row| row.get::<_, i64>(0).map(|v| v as u64),
        )
        .map_err(|e| {
            EngineError::new(
                Stage::Read,
                catalog::catalog(),
                ErrorKind::Sqlite(e.to_string()),
            )
        })?
        .ok_or_else(|| {
            EngineError::new(
                Stage::Read,
                &inst.name,
                ErrorKind::State("program vanished from catalog".into()),
            )
        })
}
