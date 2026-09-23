use crate::{Change, EngineError, Frontier, FrontierEngine, Plan, WeightedRow};
use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension, Result};
use sqlite_ext::{BulkTrigger, RowChange};
use std::collections::BTreeSet;

fn failure(stage: &'static str, source: Option<u8>, error: impl ToString) -> EngineError {
    EngineError {
        stage,
        source,
        message: error.to_string(),
    }
}

fn sql_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ModuleError(message.into())
}

fn table(source: u8) -> String {
    format!("src_{source}")
}
fn delta(source: u8) -> String {
    format!("__iso_d{source}")
}

fn sources(plan: &Plan) -> Vec<u8> {
    match plan {
        Plan::JoinUnion {
            left,
            right,
            direct,
            ..
        } => vec![*left, *right, *direct],
        Plan::GroupCountSum { source, .. } => vec![*source],
    }
}

/// Install one plan on a caller-owned connection. The fixture's native entry
/// and the linked Rust host use this exact function.
pub fn install_on(db: &Connection, plan: Plan) -> Result<()> {
    let ids = sources(&plan);
    if ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(sql_error("source IDs must be distinct"));
    }
    let positions: Vec<usize> = match &plan {
        Plan::JoinUnion {
            left_key,
            right_key,
            left_output,
            right_output,
            direct_output,
            ..
        } => vec![
            *left_key,
            *right_key,
            *left_output,
            *right_output,
            direct_output[0],
            direct_output[1],
        ],
        Plan::GroupCountSum { group, value, .. } => vec![*group, *value],
    };
    if positions.iter().any(|at| *at >= 2) {
        return Err(sql_error("this ISO storage has two value columns"));
    }
    db.execute_batch("CREATE TABLE __iso_meta(next_batch INTEGER NOT NULL); INSERT INTO __iso_meta VALUES(0); CREATE TABLE __iso_outbox(batch INTEGER NOT NULL,c0 INTEGER NOT NULL,c1 INTEGER NOT NULL,c2 INTEGER,weight INTEGER NOT NULL);")?;
    match &plan {
        Plan::JoinUnion { .. } => db.execute_batch("CREATE TABLE __iso_support(person INTEGER NOT NULL,resource INTEGER NOT NULL,weight INTEGER NOT NULL,PRIMARY KEY(person,resource));")?,
        Plan::GroupCountSum { .. } => db.execute_batch("CREATE TABLE __iso_groups(group_id INTEGER PRIMARY KEY,jobs INTEGER NOT NULL,total_cost INTEGER NOT NULL);")?,
    }
    for source in &ids {
        db.execute_batch(&format!("CREATE TABLE {}(id INTEGER PRIMARY KEY,c0 INTEGER NOT NULL,c1 INTEGER NOT NULL); CREATE TABLE {}(sign INTEGER NOT NULL,id INTEGER NOT NULL,c0 INTEGER NOT NULL,c1 INTEGER NOT NULL);", table(*source), delta(*source)))?;
    }
    if let Plan::JoinUnion {
        left,
        right,
        left_key,
        right_key,
        ..
    } = &plan
    {
        db.execute_batch(&format!("CREATE INDEX idx_src_{left}_join ON {}(c{left_key}); CREATE INDEX idx_src_{right}_join ON {}(c{right_key}); CREATE INDEX idx_d{left}_join ON {}(c{left_key}); CREATE INDEX idx_d{right}_join ON {}(c{right_key});", table(*left),table(*right),delta(*left),delta(*right)))?;
    }
    let names = ids.iter().map(|id| table(*id)).collect::<Vec<_>>();
    let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    sqlite_ext::watch(db, "iso_watch", &refs, Maintain { plan })
}

/// Fixture registration proving the same installed plan works through a
/// loadable SQLite plugin. These SQL functions choose the two fixed oracles.
pub fn register_native_fixture(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"engine_iso_access_install",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            install_on(
                &db,
                Plan::JoinUnion {
                    left: 0,
                    right: 1,
                    direct: 2,
                    left_key: 1,
                    right_key: 0,
                    left_output: 0,
                    right_output: 1,
                    direct_output: [0, 1],
                },
            )?;
            Ok(1_i64)
        },
    )?;
    db.create_scalar_function(
        c"engine_iso_group_install",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            install_on(
                &db,
                Plan::GroupCountSum {
                    source: 0,
                    group: 0,
                    value: 1,
                },
            )?;
            Ok(1_i64)
        },
    )
}

fn int(value: &rusqlite::types::Value) -> Result<i64> {
    match value {
        rusqlite::types::Value::Integer(value) => Ok(*value),
        _ => Err(sql_error("source row requires INTEGER cells")),
    }
}

fn maintenance_sql(plan: &Plan) -> String {
    match plan {
        Plan::JoinUnion {
            left,
            right,
            direct,
            left_key,
            right_key,
            left_output,
            right_output,
            direct_output,
        } => format!(
            "SELECT person,resource,SUM(weight) FROM (\
               SELECT dl.c{left_output} AS person,p.c{right_output} AS resource,dl.sign AS weight \
                 FROM {dl} dl JOIN {right_table} p ON dl.c{left_key}=p.c{right_key} \
               UNION ALL SELECT m.c{left_output},dr.c{right_output},dr.sign \
                 FROM {left_table} m JOIN {dr} dr ON m.c{left_key}=dr.c{right_key} \
               UNION ALL SELECT dl.c{left_output},dr.c{right_output},-dl.sign*dr.sign \
                 FROM {dl} dl JOIN {dr} dr ON dl.c{left_key}=dr.c{right_key} \
               UNION ALL SELECT dd.c{direct0},dd.c{direct1},dd.sign FROM {dd} dd\
             ) GROUP BY person,resource HAVING SUM(weight)<>0",
            dl = delta(*left),
            dr = delta(*right),
            dd = delta(*direct),
            left_table = table(*left),
            right_table = table(*right),
            direct0 = direct_output[0],
            direct1 = direct_output[1],
        ),
        Plan::GroupCountSum {
            source,
            group,
            value,
        } => format!(
            "SELECT c{group},SUM(sign),SUM(sign*c{value}) FROM {} GROUP BY c{group}",
            delta(*source)
        ),
    }
}

struct Maintain {
    plan: Plan,
}

impl Maintain {
    fn feed(&self, db: &Connection, batch: &[RowChange]) -> Result<()> {
        let _span = tracing::info_span!("sqlite_frontier", inputs = batch.len()).entered();
        let affected = sources(&self.plan);
        for source in &affected {
            db.execute(&format!("DELETE FROM {}", delta(*source)), [])?;
        }
        for change in batch {
            let source: u8 = change
                .table
                .strip_prefix("src_")
                .ok_or_else(|| sql_error("collector source name"))?
                .parse()
                .map_err(|_| sql_error("collector source ID"))?;
            if !affected.contains(&source) {
                return Err(sql_error("collector source absent from plan"));
            }
            if change.values.len() != 3 {
                return Err(sql_error("source row width"));
            }
            db.prepare_cached(&format!(
                "INSERT INTO {}(sign,id,c0,c1) VALUES(?1,?2,?3,?4)",
                delta(source)
            ))?
            .execute(params![
                change.sign.as_integer(),
                int(&change.values[0])?,
                int(&change.values[1])?,
                int(&change.values[2])?
            ])?;
        }
        let batch_id: i64 =
            db.query_row("SELECT next_batch FROM __iso_meta", [], |row| row.get(0))?;
        db.execute("UPDATE __iso_meta SET next_batch = next_batch + 1", [])?;
        match &self.plan {
            Plan::JoinUnion { .. } => self.join_union(db, batch_id)?,
            Plan::GroupCountSum { .. } => self.group_count_sum(db, batch_id)?,
        }
        for source in &affected {
            db.execute(&format!("DELETE FROM {}", delta(*source)), [])?;
        }
        tracing::info!(batch_id, "sqlite_frontier_settled");
        Ok(())
    }

    fn join_union(&self, db: &Connection, batch_id: i64) -> Result<()> {
        let query = maintenance_sql(&self.plan);
        tracing::debug!(sql_bytes = query.len(), sql = %query, "sqlite_join_delta_sql");
        let contributions = db
            .prepare_cached(&query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        for (person, resource, weight) in contributions {
            let old: i64 = db
                .query_row(
                    "SELECT weight FROM __iso_support WHERE person=?1 AND resource=?2",
                    params![person, resource],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let new = old
                .checked_add(weight)
                .ok_or_else(|| sql_error("support overflow"))?;
            if new < 0 {
                return Err(sql_error("negative support"));
            }
            if old == 0 && new > 0 {
                db.execute(
                    "INSERT INTO __iso_outbox VALUES(?1,?2,?3,NULL,1)",
                    params![batch_id, person, resource],
                )?;
            }
            if old > 0 && new == 0 {
                db.execute(
                    "INSERT INTO __iso_outbox VALUES(?1,?2,?3,NULL,-1)",
                    params![batch_id, person, resource],
                )?;
            }
            if new == 0 {
                db.execute(
                    "DELETE FROM __iso_support WHERE person=?1 AND resource=?2",
                    params![person, resource],
                )?;
            } else {
                db.execute("INSERT INTO __iso_support(person,resource,weight) VALUES(?1,?2,?3) ON CONFLICT(person,resource) DO UPDATE SET weight=excluded.weight", params![person, resource, new])?;
            }
        }
        Ok(())
    }

    fn group_count_sum(&self, db: &Connection, batch_id: i64) -> Result<()> {
        let query = maintenance_sql(&self.plan);
        tracing::debug!(sql_bytes = query.len(), sql = %query, "sqlite_group_delta_sql");
        let contributions = db
            .prepare_cached(&query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        for (key, count, sum) in contributions {
            let old: (i64, i64) = db
                .query_row(
                    "SELECT jobs,total_cost FROM __iso_groups WHERE group_id=?1",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .unwrap_or_default();
            let new_count = old
                .0
                .checked_add(count)
                .ok_or_else(|| sql_error("group count overflow"))?;
            let new_sum = old
                .1
                .checked_add(sum)
                .ok_or_else(|| sql_error("group sum overflow"))?;
            if new_count < 0 {
                return Err(sql_error("negative group count"));
            }
            if old == (new_count, new_sum) {
                continue;
            }
            if old.0 > 0 {
                db.execute(
                    "INSERT INTO __iso_outbox VALUES(?1,?2,?3,?4,-1)",
                    params![batch_id, key, old.0, old.1],
                )?;
            }
            if new_count > 0 {
                db.execute(
                    "INSERT INTO __iso_outbox VALUES(?1,?2,?3,?4,1)",
                    params![batch_id, key, new_count, new_sum],
                )?;
            }
            if new_count == 0 {
                db.execute("DELETE FROM __iso_groups WHERE group_id=?1", [key])?;
            } else {
                db.execute("INSERT INTO __iso_groups(group_id,jobs,total_cost) VALUES(?1,?2,?3) ON CONFLICT(group_id) DO UPDATE SET jobs=excluded.jobs,total_cost=excluded.total_cost", params![key,new_count,new_sum])?;
            }
        }
        Ok(())
    }
}

impl BulkTrigger for Maintain {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> Result<()> {
        self.feed(db, batch)
    }
}

pub struct SqliteEngine {
    db: Connection,
    plan: Option<Plan>,
    next_frontier: u64,
}

impl SqliteEngine {
    pub fn memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    fn from_connection(db: Connection) -> Result<Self> {
        const OBSERVER: sqlite_ext::Plugin =
            sqlite_ext::Plugin::new("engine_iso", env!("CARGO_PKG_VERSION"), "warn", |_| Ok(()));
        OBSERVER.register(&db)?;
        db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;")?;
        Ok(Self {
            db,
            plan: None,
            next_frontier: 0,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.db
    }

    pub fn maintenance_sql(&self) -> std::result::Result<String, EngineError> {
        self.plan
            .as_ref()
            .map(maintenance_sql)
            .ok_or_else(|| failure("inspect", None, "program not installed"))
    }

    pub fn explain_maintenance(&self) -> std::result::Result<Vec<String>, EngineError> {
        let sql = format!("EXPLAIN QUERY PLAN {}", self.maintenance_sql()?);
        self.db
            .prepare(&sql)
            .map_err(|error| failure("inspect", None, error))?
            .query_map([], |row| row.get::<_, String>(3))
            .map_err(|error| failure("inspect", None, error))?
            .collect::<Result<Vec<_>>>()
            .map_err(|error| failure("inspect", None, error))
    }

    fn apply_inner(&self, changes: &[Change]) -> Result<Vec<WeightedRow>> {
        let before: i64 = self
            .db
            .query_row("SELECT next_batch FROM __iso_meta", [], |row| row.get(0))?;
        self.db.execute_batch("BEGIN")?;
        let write = (|| -> Result<()> {
            for change in changes {
                if change.weight != 1 && change.weight != -1 {
                    return Err(sql_error("weight must be +1 or -1"));
                }
                if change.row.cells.len() != 2 {
                    return Err(sql_error("source row must have two cells"));
                }
                let name = table(change.source);
                if change.weight == 1 {
                    self.db
                        .prepare_cached(&format!("INSERT INTO {name}(id,c0,c1) VALUES(?1,?2,?3)"))?
                        .execute(params![
                            change.row.id,
                            change.row.cells[0],
                            change.row.cells[1]
                        ])?;
                } else {
                    let removed = self
                        .db
                        .prepare_cached(&format!(
                            "DELETE FROM {name} WHERE id=?1 AND c0=?2 AND c1=?3"
                        ))?
                        .execute(params![
                            change.row.id,
                            change.row.cells[0],
                            change.row.cells[1]
                        ])?;
                    if removed != 1 {
                        return Err(sql_error("retraction does not match a stored row"));
                    }
                }
            }
            self.db.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(error) = write {
            let _ = self.db.execute_batch("ROLLBACK");
            return Err(error);
        }
        let after: i64 = self
            .db
            .query_row("SELECT next_batch FROM __iso_meta", [], |row| row.get(0))?;
        if before == after {
            return Ok(Vec::new());
        }
        let grouped = matches!(self.plan, Some(Plan::GroupCountSum { .. }));
        let changes = self
            .db
            .prepare_cached(
                "SELECT c0,c1,c2,weight FROM __iso_outbox WHERE batch=?1 ORDER BY c0,c1,c2,weight",
            )?
            .query_map([before], |row| {
                let mut cells = vec![row.get::<_, i64>(0)?, row.get::<_, i64>(1)?];
                if grouped {
                    cells.push(row.get::<_, i64>(2)?);
                }
                Ok(WeightedRow {
                    cells,
                    weight: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        self.db
            .execute("DELETE FROM __iso_outbox WHERE batch=?1", [before])?;
        Ok(changes)
    }
}

impl FrontierEngine for SqliteEngine {
    type Plan = Plan;
    type Change = Change;
    type Output = Vec<i64>;
    type Error = EngineError;

    fn install(&mut self, plan: Plan) -> std::result::Result<(), EngineError> {
        if self.plan.is_some() {
            return Err(failure("install", None, "program already installed"));
        }
        install_on(&self.db, plan.clone()).map_err(|error| failure("install", None, error))?;
        self.plan = Some(plan);
        Ok(())
    }

    fn apply(&mut self, changes: &[Change]) -> std::result::Result<Frontier, EngineError> {
        if self.plan.is_none() {
            return Err(failure("apply", None, "program not installed"));
        }
        let out = self.apply_inner(changes).map_err(|error| {
            failure("apply", changes.first().map(|change| change.source), error)
        })?;
        let frontier = Frontier {
            id: self.next_frontier,
            changes: out,
        };
        self.next_frontier += 1;
        hafley_observe::sqlite_memory::record_memory(&self.db, "frontier_settled");
        Ok(frontier)
    }

    fn snapshot(&self) -> std::result::Result<Vec<Vec<i64>>, EngineError> {
        let Some(plan) = &self.plan else {
            return Err(failure("read", None, "program not installed"));
        };
        let query = match plan {
            Plan::JoinUnion { .. } => {
                "SELECT person,resource FROM __iso_support WHERE weight>0 ORDER BY person,resource"
            }
            Plan::GroupCountSum { .. } => {
                "SELECT group_id,jobs,total_cost FROM __iso_groups ORDER BY group_id"
            }
        };
        self.db
            .prepare_cached(query)
            .map_err(|error| failure("read", None, error))?
            .query_map([], |row| {
                let width = if matches!(plan, Plan::JoinUnion { .. }) {
                    2
                } else {
                    3
                };
                (0..width)
                    .map(|at| row.get::<_, i64>(at))
                    .collect::<Result<Vec<_>>>()
            })
            .map_err(|error| failure("read", None, error))?
            .collect::<Result<Vec<_>>>()
            .map_err(|error| failure("read", None, error))
    }

    fn teardown(&mut self) -> std::result::Result<(), EngineError> {
        let Some(plan) = self.plan.take() else {
            return Ok(());
        };
        self.db
            .execute_batch("DROP TABLE iso_watch;")
            .map_err(|error| failure("teardown", None, error))?;
        for source in sources(&plan) {
            self.db
                .execute_batch(&format!(
                    "DROP TABLE {}; DROP TABLE {};",
                    table(source),
                    delta(source)
                ))
                .map_err(|error| failure("teardown", Some(source), error))?;
        }
        let state = match plan {
            Plan::JoinUnion { .. } => "DROP TABLE __iso_support;",
            Plan::GroupCountSum { .. } => "DROP TABLE __iso_groups;",
        };
        self.db
            .execute_batch(&format!(
                "{state} DROP TABLE __iso_outbox; DROP TABLE __iso_meta;"
            ))
            .map_err(|error| failure("teardown", None, error))?;
        self.next_frontier = 0;
        Ok(())
    }
}
