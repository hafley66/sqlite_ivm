//! Host-backed typed Engine adapter. The handle keeps no SQLite connection.

use crate::{
    catalog, Cell, Frontier as SqlFrontier, Program as SqlProgram, Sign, SourceChange as SqlChange,
};
use ivm_engine::{Engine, EngineError, ErrorKind, Host, Stage};
use ivm_ir::{Delta, Frontier, Op, Program, RelId, RelKind, Row, Stratum, W};
use sqlite_ext::rusqlite::{self, Connection};

pub struct Sqlite {
    programs: Vec<OutputProgram>,
    ir: Program,
    tick: u64,
}

struct OutputProgram {
    rel: RelId,
    program: SqlProgram,
    threshold: bool,
}

fn error(stage: Stage, e: impl std::fmt::Display) -> EngineError {
    EngineError::new(stage, None, ErrorKind::Worker(e.to_string()))
}

fn plan_error(e: crate::EngineError) -> EngineError {
    match e.kind {
        crate::ErrorKind::Unsupported(why) => EngineError::new(Stage::Install, None, ErrorKind::Unsupported(why)),
        _ => error(Stage::Install, e),
    }
}

fn conn(host: &mut impl Host, stage: Stage) -> Result<&Connection, EngineError> {
    host.conn()
        .and_then(|c| c.downcast_ref::<Connection>())
        .ok_or_else(|| {
            EngineError::new(
                stage,
                None,
                ErrorKind::Unsupported("SQLite Connection host required"),
            )
        })
}

fn row_of(row: &rusqlite::Row<'_>, width: usize) -> rusqlite::Result<Row> {
    (0..width).map(|i| row.get(i)).collect()
}

impl Engine for Sqlite {
    fn install(ir: &Program, host: &mut impl Host) -> Result<Self, EngineError> {
        let db = conn(host, Stage::Install)?;
        if ir.outputs.is_empty() {
            return Err(EngineError::new(
                Stage::Install,
                None,
                ErrorKind::Unsupported("output relation required"),
            ));
        }
        for source in ir.rels.iter().filter(|r| r.kind == RelKind::Source) {
            let columns = (0..source.cols.len())
                .map(|i| format!("c{i} INTEGER NOT NULL"))
                .collect::<Vec<_>>()
                .join(",");
            db.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {}({columns})",
                catalog::quote(&source.name)
            ))
            .map_err(|e| error(Stage::Install, e))?;
            let keys = (0..source.cols.len()).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
            db.execute_batch(&format!(
                "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {}({keys})",
                catalog::quote(format!("ivm_host_{}_set", source.name)),
                catalog::quote(&source.name),
            )).map_err(|e| error(Stage::Install, e))?;
        }
        let mut programs = Vec::new();
        for &output in &ir.outputs {
            let name = ir
                .rel(output)
                .map(|r| r.name.as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("ivm_output_{output}"));
            let mut single = ir.clone();
            single.outputs = vec![output];
            let program = SqlProgram::install_ir_unwatched(db, &name, &single)
                .map_err(plan_error)?;
            let threshold = matches!(ir.strata.as_slice(), [Stratum::Let { id, body }]
                if *id == output && matches!(ir.nodes.get(*body as usize), Some(Op::Threshold(_))));
            programs.push(OutputProgram {
                rel: output,
                program,
                threshold,
            });
        }
        Ok(Self {
            programs,
            ir: ir.clone(),
            tick: 0,
        })
    }

    fn settle(&mut self, frontier: Frontier, host: &mut impl Host) -> Result<Delta, EngineError> {
        let db = conn(host, Stage::Settle)?;
        db.execute_batch("SAVEPOINT ivm_engine_frontier;")
            .map_err(|e| error(Stage::Settle, e))?;
        let run = || -> Result<Vec<(RelId, Row, W)>, EngineError> {
            let mut batch = Vec::new();
            for change in &frontier.changes {
                let source = self
                    .ir
                    .rel(change.rel)
                    .filter(|r| r.kind == RelKind::Source)
                    .ok_or_else(|| {
                        EngineError::new(
                            Stage::Settle,
                            Some(change.rel),
                            ErrorKind::UnknownRel(change.rel),
                        )
                    })?;
                if change.row.len() != source.cols.len() {
                    return Err(EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::Arity {
                            expected: source.cols.len(),
                            actual: change.row.len(),
                        },
                    ));
                }
                if change.w != 1 && change.w != -1 {
                    return Err(EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::Unsupported("weight other than +1/-1"),
                    ));
                }
                let names = self.programs[0]
                    .program
                    .inner
                    .plan
                    .scans
                    .iter()
                    .find(|s| s.table == source.name)
                    .map(|s| s.columns.as_slice())
                    .ok_or_else(|| error(Stage::Settle, "source scan missing"))?;
                let match_row = names
                    .iter()
                    .map(|c| format!("{}=?", catalog::quote(c)))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let table = catalog::quote(&source.name);
                let count: i64 = db
                    .query_row(
                        &format!("SELECT count(*) FROM {table} WHERE {match_row}"),
                        rusqlite::params_from_iter(&change.row),
                        |r| r.get(0),
                    )
                    .map_err(|e| error(Stage::Settle, e))?;
                if change.w > 0 && count > 0 {
                    return Err(EngineError::new(
                        Stage::Settle,
                        Some(change.rel),
                        ErrorKind::PresentInsert(change.row.clone()),
                    ));
                }
                if change.w < 0 && count == 0 {
                    continue;
                }
                let sql = if change.w > 0 {
                    format!(
                        "INSERT INTO {table} VALUES ({})",
                        vec!["?"; names.len()].join(",")
                    )
                } else {
                    format!("DELETE FROM {table} WHERE {match_row}")
                };
                db.execute(&sql, rusqlite::params_from_iter(&change.row))
                    .map_err(|e| error(Stage::Settle, e))?;
                batch.push(SqlChange {
                    relation: source.name.clone(),
                    sign: if change.w > 0 {
                        Sign::Insert
                    } else {
                        Sign::Delete
                    },
                    row: change.row.iter().copied().map(Cell::Integer).collect(),
                });
            }
            let mut changes = Vec::new();
            for output in &self.programs {
                let visible = output
                    .program
                    .settle(db, &batch)
                    .map_err(|e| error(Stage::Settle, e))?;
                if output.threshold || output.program.inner.sqls.weight_delta.is_none() {
                    for change in visible {
                        let row = change
                            .row
                            .into_iter()
                            .map(|cell| match cell {
                                Cell::Integer(v) => Ok(v),
                                _ => Err(error(Stage::Settle, "non-integer output")),
                            })
                            .collect::<Result<Row, _>>()?;
                        changes.push((output.rel, row, change.sign.as_integer()));
                    }
                } else {
                    let width = output.program.inner.output.len();
                    let mut stmt = db
                        .prepare(output.program.inner.sqls.weight_delta.as_deref().unwrap())
                        .map_err(|e| error(Stage::Settle, e))?;
                    let rows = stmt
                        .query_map([], |r| Ok((row_of(r, width)?, r.get::<_, W>(width)?)))
                        .map_err(|e| error(Stage::Settle, e))?;
                    for row in rows {
                        let (row, weight) = row.map_err(|e| error(Stage::Settle, e))?;
                        changes.push((output.rel, row, weight));
                    }
                }
            }
            changes.sort();
            Ok(changes)
        };
        match run() {
            Ok(changes) => {
                db.execute_batch("RELEASE ivm_engine_frontier;")
                    .map_err(|e| error(Stage::Settle, e))?;
                let tick = self.tick;
                self.tick += 1;
                Ok(Delta { tick, changes })
            }
            Err(e) => {
                let _ = db
                    .execute_batch("ROLLBACK TO ivm_engine_frontier; RELEASE ivm_engine_frontier;");
                Err(e)
            }
        }
    }

    fn snapshot(&self, rel: RelId, host: &mut impl Host) -> Result<Vec<(Row, W)>, EngineError> {
        let output = self
            .programs
            .iter()
            .find(|output| output.rel == rel)
            .ok_or_else(|| {
                EngineError::new(Stage::Snapshot, Some(rel), ErrorKind::UnknownRel(rel))
            })?;
        let db = conn(host, Stage::Snapshot)?;
        let plan = &output.program.inner.plan;
        let width = plan.output.len();
        let sql = match &plan.root {
            crate::plan::Root::Nodes(nodes) => nodes.output_snapshot.clone(),
            crate::plan::Root::Union { .. } => {
                let cols = plan
                    .output
                    .iter()
                    .map(|c| catalog::quote(&c.name))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "SELECT {cols},__weight FROM {} WHERE __weight>0 ORDER BY {cols}",
                    catalog::quote(catalog::root(&output.program.inner.name))
                )
            }
            crate::plan::Root::Group { .. } => {
                let snapshot = output.program.inner.sqls.snapshot.trim_end_matches(';');
                format!("SELECT *,1 FROM ({snapshot})")
            }
        };
        let mut stmt = db.prepare(&sql).map_err(|e| error(Stage::Snapshot, e))?;
        let result = stmt
            .query_map([], |r| {
                let row = row_of(r, width)?;
                let weight: W = r.get(width)?;
                Ok((
                    row,
                    if output.threshold {
                        weight.min(1)
                    } else {
                        weight
                    },
                ))
            })
            .map_err(|e| error(Stage::Snapshot, e))?
            .map(|r| r.map_err(|e| error(Stage::Snapshot, e)))
            .collect();
        result
    }
}
