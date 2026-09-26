//! One frontier stream and recompute oracle across six arms.
//! Run `just frontier-stress --rows=32,1024 --batch=1,8 --fanout=1,16`.

use anyhow::{bail, Context, Result};
use frontier_dd_packet as dd_packet;
use ivm_sqlite::Program;
use lab_20260923_0 as direct;
use lab_20260923_1 as rust_iso;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const ACCESS: &str = "SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team";
const GROUP: &str = "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team";
static NEXT_DB: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let id = NEXT_DB.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir()
                .join(format!("frontier-stress-{}-{id}.db", std::process::id())),
        }
    }

    fn bytes(&self) -> (u64, u64) {
        let size =
            |path: &std::path::Path| std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        (size(&self.path), size(&self.path.with_extension("db-wal")))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for path in [
            &self.path,
            &self.path.with_extension("db-wal"),
            &self.path.with_extension("db-shm"),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Access,
    Group,
}

#[derive(Clone, Copy, Debug)]
enum SqliteMode {
    IsoRust,
    IsoExtension,
    MainFrontier,
    DdExtension,
    Production,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table {
    Membership,
    Permission,
    Direct,
    Job,
}

impl Table {
    fn sql(self) -> &'static str {
        match self {
            Self::Membership => "membership",
            Self::Permission => "permission",
            Self::Direct => "direct_grant",
            Self::Job => "job",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Membership => 0,
            Self::Permission => 1,
            Self::Direct => 2,
            Self::Job => 3,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Change {
    table: Table,
    sign: i64,
    id: i64,
    a: i64,
    b: i64,
}

#[derive(Clone, Debug)]
struct Phase {
    name: &'static str,
    changes: Vec<Change>,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    shape: Shape,
    rows: i64,
    batch: i64,
    fanout: i64,
    hot: bool,
    churn: i64,
    check_every: i64,
}

#[derive(Default)]
struct Oracle {
    rows: [BTreeMap<i64, (i64, i64)>; 4],
}

impl Oracle {
    fn apply(&mut self, phase: &Phase) -> Result<()> {
        for change in &phase.changes {
            let table = &mut self.rows[change.table.index()];
            if change.sign > 0 {
                if table.insert(change.id, (change.a, change.b)).is_some() {
                    bail!("duplicate source id {} in {}", change.id, phase.name);
                }
            } else if table.remove(&change.id) != Some((change.a, change.b)) {
                bail!("bad removal {} in {}", change.id, phase.name);
            }
        }
        Ok(())
    }

    fn snapshot(&self, shape: Shape) -> Vec<Vec<i64>> {
        match shape {
            Shape::Access => {
                let permissions: BTreeMap<i64, Vec<i64>> = self.rows[1].values().fold(
                    BTreeMap::new(),
                    |mut by_team, &(team, resource)| {
                        by_team.entry(team).or_default().push(resource);
                        by_team
                    },
                );
                let mut out: BTreeSet<Vec<i64>> = self.rows[2]
                    .values()
                    .map(|&(person, resource)| vec![person, resource])
                    .collect();
                for &(person, team) in self.rows[0].values() {
                    if let Some(resources) = permissions.get(&team) {
                        for &resource in resources {
                            out.insert(vec![person, resource]);
                        }
                    }
                }
                out.into_iter().collect()
            }
            Shape::Group => {
                let mut groups: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
                for &(team, cost) in self.rows[3].values() {
                    let entry = groups.entry(team).or_default();
                    entry.0 += 1;
                    entry.1 += cost;
                }
                groups
                    .into_iter()
                    .map(|(team, (count, sum))| vec![team, count, sum])
                    .collect()
            }
        }
    }
}

fn fixture(cell: Cell) -> Vec<Phase> {
    let mut seed = Vec::new();
    let mut batch = Vec::new();
    let mut churn = Vec::new();
    match cell.shape {
        Shape::Access => {
            let teams = (cell.rows / cell.fanout).max(1);
            for id in 0..cell.rows {
                seed.push(Change {
                    table: Table::Membership,
                    sign: 1,
                    id,
                    a: id,
                    b: id % teams,
                });
                seed.push(Change {
                    table: Table::Permission,
                    sign: 1,
                    id,
                    a: id % teams,
                    b: 100_000 + id,
                });
            }
            for id in 0..cell.rows / 8 {
                seed.push(Change {
                    table: Table::Direct,
                    sign: 1,
                    id,
                    a: id,
                    b: 200_000 + id,
                });
            }
            for i in 0..cell.batch {
                batch.push(Change {
                    table: Table::Membership,
                    sign: 1,
                    id: cell.rows + i,
                    a: cell.rows + i,
                    b: 0,
                });
                batch.push(Change {
                    table: Table::Permission,
                    sign: 1,
                    id: cell.rows + i,
                    a: 0,
                    b: 300_000 + i,
                });
            }
            for i in 0..cell.churn {
                let row = Change {
                    table: Table::Direct,
                    sign: 1,
                    id: cell.rows + i,
                    a: 900_000 + i,
                    b: 400_000 + i,
                };
                churn.push(Phase {
                    name: "churn",
                    changes: vec![row],
                });
                churn.push(Phase {
                    name: "churn",
                    changes: vec![Change { sign: -1, ..row }],
                });
            }
        }
        Shape::Group => {
            for id in 0..cell.rows {
                seed.push(Change {
                    table: Table::Job,
                    sign: 1,
                    id,
                    a: if cell.hot { 0 } else { id },
                    b: id % 17 + 1,
                });
            }
            for i in 0..cell.batch {
                batch.push(Change {
                    table: Table::Job,
                    sign: 1,
                    id: cell.rows + i,
                    a: if cell.hot { 0 } else { cell.rows + i },
                    b: i % 17 + 1,
                });
            }
            for i in 0..cell.churn {
                let id = i % cell.rows;
                let a = if cell.hot { 0 } else { id };
                let old = id % 17 + 1;
                churn.push(Phase {
                    name: "churn",
                    changes: vec![
                        Change {
                            table: Table::Job,
                            sign: -1,
                            id,
                            a,
                            b: old,
                        },
                        Change {
                            table: Table::Job,
                            sign: 1,
                            id,
                            a,
                            b: old + 1,
                        },
                    ],
                });
                churn.push(Phase {
                    name: "churn",
                    changes: vec![
                        Change {
                            table: Table::Job,
                            sign: -1,
                            id,
                            a,
                            b: old + 1,
                        },
                        Change {
                            table: Table::Job,
                            sign: 1,
                            id,
                            a,
                            b: old,
                        },
                    ],
                });
            }
        }
    }
    let mut phases = vec![
        Phase {
            name: "seed",
            changes: seed,
        },
        Phase {
            name: "batch",
            changes: batch,
        },
    ];
    phases.extend(churn);
    phases
}

#[derive(Default)]
struct Measure {
    install: Duration,
    seed: Duration,
    batch: Duration,
    churn: Duration,
    read: Duration,
    rows: usize,
    sqlite_bytes: Option<i64>,
    rss: Option<u64>,
    db_bytes: Option<u64>,
    wal_bytes: Option<u64>,
    batch_vm: Option<u64>,
    batch_statements: Option<usize>,
    batch_fullscan: Option<u64>,
}

fn check(
    got: Vec<Vec<i64>>,
    expected: Vec<Vec<i64>>,
    arm: &str,
    cell: Cell,
    step: usize,
) -> Result<usize> {
    if got != expected {
        bail!(
            "{arm} mismatch at step {step} {cell:?}: got {} rows {:?}, expected {} rows {:?}",
            got.len(),
            got.iter().take(8).collect::<Vec<_>>(),
            expected.len(),
            expected.iter().take(8).collect::<Vec<_>>()
        );
    }
    Ok(got.len())
}

fn expected_delta(before: Vec<Vec<i64>>, after: &[Vec<i64>]) -> Vec<Vec<i64>> {
    let before: BTreeSet<_> = before.into_iter().collect();
    let after: BTreeSet<_> = after.iter().cloned().collect();
    let mut changes: Vec<_> = before
        .difference(&after)
        .map(|row| std::iter::once(-1).chain(row.iter().copied()).collect())
        .chain(
            after
                .difference(&before)
                .map(|row| std::iter::once(1).chain(row.iter().copied()).collect()),
        )
        .collect();
    changes.sort();
    changes
}

fn run_sqlite(
    cell: Cell,
    phases: &[Phase],
    mode: SqliteMode,
    observe: bool,
    file: bool,
) -> Result<Measure> {
    let install_start = Instant::now();
    let file_db = file.then(TempDb::new);
    let db = if let Some(file_db) = &file_db {
        Connection::open(&file_db.path)?
    } else {
        Connection::open_in_memory()?
    };
    if file {
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    }
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON; CREATE TABLE membership(id INTEGER PRIMARY KEY,person INTEGER,team INTEGER); CREATE TABLE permission(id INTEGER PRIMARY KEY,team INTEGER,resource INTEGER); CREATE TABLE direct_grant(id INTEGER PRIMARY KEY,person INTEGER,resource INTEGER); CREATE TABLE job(id INTEGER PRIMARY KEY,team INTEGER,cost INTEGER);")?;
    let query = if cell.shape == Shape::Access {
        ACCESS
    } else {
        GROUP
    };
    let view = if matches!(mode, SqliteMode::IsoExtension) {
        let path = std::env::var_os("FRONTIER_EXT_PATH")
            .context("FRONTIER_EXT_PATH must name the built frontier-ext library")?;
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(path, Some("sqlite3_frontier_ext_init"))?;
            db.load_extension_disable()?;
        }
        let name = if cell.shape == Shape::Access {
            "access"
        } else {
            "team_cost"
        };
        db.query_row("SELECT frontier_install(?1, ?2)", (name, query), |row| {
            row.get::<_, i64>(0)
        })?;
        format!("frontier_{name}")
    } else if matches!(mode, SqliteMode::MainFrontier) {
        sqlite_ivm::extension::register(&db)?;
        let name = if cell.shape == Shape::Access {
            "access"
        } else {
            "team_cost"
        };
        db.query_row(
            "SELECT sqlite_ivm_frontier_install(?1, ?2)",
            (name, query),
            |row| row.get::<_, String>(0),
        )?;
        format!("frontier_{name}")
    } else if matches!(mode, SqliteMode::DdExtension) {
        let path = std::env::var_os("FRONTIER_DD_EXT_PATH")
            .context("FRONTIER_DD_EXT_PATH must name the built frontier-dd-ext library")?;
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(path, Some("sqlite3_frontier_dd_ext_init"))?;
            db.load_extension_disable()?;
        }
        let name = if cell.shape == Shape::Access {
            "access"
        } else {
            "team_cost"
        };
        db.query_row("SELECT dd_frontier_install(?1)", [name], |row| {
            row.get::<_, i64>(0)
        })?;
        format!("dd_frontier_{name}")
    } else if matches!(mode, SqliteMode::IsoRust) {
        let name = if cell.shape == Shape::Access {
            "access"
        } else {
            "team_cost"
        };
        let _program = Program::install(&db, name, query)?;
        format!("frontier_{name}")
    } else {
        sqlite_ivm::extension::register(&db)?;
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
            query.replace('\'', "''")
        ))?;
        "result".to_string()
    };
    let mut measured = Measure {
        install: install_start.elapsed(),
        ..Measure::default()
    };
    let (recorder, _guard) = if observe {
        let (recorder, layer) = hafley_observe::CountRecorder::new();
        let guard = tracing_subscriber::registry().with(layer).set_default();
        hafley_observe::sqlite::instrument(&db);
        (Some(recorder), Some(guard))
    } else {
        (None, None)
    };
    let mut oracle = Oracle::default();
    for (step, phase) in phases.iter().enumerate() {
        let inspect = step < 2 || step as i64 % cell.check_every == 0 || step + 1 == phases.len();
        let before = inspect.then(|| oracle.snapshot(cell.shape));
        let start = Instant::now();
        let span = tracing::info_span!("stress_phase", phase = phase.name);
        let entered = span.enter();
        db.execute_batch("BEGIN")?;
        for change in &phase.changes {
            if change.sign > 0 {
                db.execute(
                    &format!("INSERT INTO {} VALUES(?1,?2,?3)", change.table.sql()),
                    (change.id, change.a, change.b),
                )?;
            } else {
                db.execute(
                    &format!("DELETE FROM {} WHERE id=?1", change.table.sql()),
                    [change.id],
                )?;
            }
        }
        db.execute_batch("COMMIT")?;
        drop(entered);
        record_apply(&mut measured, phase.name, start.elapsed());
        oracle.apply(phase)?;
        if inspect {
            let expected = oracle.snapshot(cell.shape);
            let start = Instant::now();
            let cols = if cell.shape == Shape::Access { 2 } else { 3 };
            let sql = format!(
                "SELECT * FROM {view} ORDER BY {}",
                if cols == 2 { "1,2" } else { "1,2,3" }
            );
            let mut stmt = db.prepare(&sql)?;
            let got = stmt
                .query_map([], |row| {
                    (0..cols)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<i64>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            measured.read += start.elapsed();
            measured.rows = check(
                got,
                expected.clone(),
                match mode {
                    SqliteMode::IsoRust => "sqlite-iso",
                    SqliteMode::IsoExtension => "sqlite-iso-ext",
                    SqliteMode::MainFrontier => "sqlite-ivm-frontier",
                    SqliteMode::DdExtension => "dd-ext",
                    SqliteMode::Production => "sqlite-ivm",
                },
                cell,
                step,
            )?;
            if !matches!(mode, SqliteMode::Production) {
                let delta_start = Instant::now();
                let name = if cell.shape == Shape::Access {
                    "access"
                } else {
                    "team_cost"
                };
                let prefix = if matches!(mode, SqliteMode::DdExtension) {
                    "dd_frontier"
                } else {
                    "frontier"
                };
                let mut delta = db.prepare(&format!("SELECT * FROM {prefix}_{name}_delta"))?;
                let got = delta
                    .query_map([], |row| {
                        (0..cols + 1)
                            .map(|column| row.get(column))
                            .collect::<rusqlite::Result<Vec<i64>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                measured.read += delta_start.elapsed();
                let mut got = got;
                got.sort();
                let wanted = expected_delta(before.expect("checked frontier"), &expected);
                if got != wanted {
                    bail!("{mode:?} signed delta mismatch at step {step}: got {got:?}, expected {wanted:?}");
                }
            }
        }
    }
    if let Some(recorder) = recorder {
        hafley_observe::sqlite::silence(&db);
        let sums = recorder.event_sums(
            hafley_observe::sqlite::SQLITE_TARGET,
            tracing::Level::DEBUG,
            "stress_phase",
            "phase",
            None,
        );
        let batch = sums.get(&(String::from("batch"), String::new()));
        measured.batch_vm = Some(batch.map_or(0, |s| s.sum_of("vm_step") as u64));
        measured.batch_statements = Some(batch.map_or(0, |s| s.events));
        measured.batch_fullscan = Some(batch.map_or(0, |s| s.sum_of("fullscan_step") as u64));
    }
    measured.sqlite_bytes =
        hafley_observe::sqlite_memory::sample(&db).map(|m| m.allocator_current_bytes);
    if let Some(file_db) = &file_db {
        let (db_bytes, wal_bytes) = file_db.bytes();
        measured.db_bytes = Some(db_bytes);
        measured.wal_bytes = Some(wal_bytes);
    }
    measured.rss = hafley_observe::rusage::sample().peak_rss_bytes;
    Ok(measured)
}

fn record_apply(measured: &mut Measure, phase: &str, elapsed: Duration) {
    match phase {
        "seed" => measured.seed += elapsed,
        "batch" => measured.batch += elapsed,
        _ => measured.churn += elapsed,
    }
}

fn direct_plan(shape: Shape) -> direct::Plan {
    match shape {
        Shape::Access => direct::Plan::JoinUnion {
            left: 0,
            right: 1,
            direct: 2,
            left_key: 1,
            right_key: 0,
            left_output: 0,
            right_output: 1,
            direct_output: [0, 1],
        },
        Shape::Group => direct::Plan::GroupCountSum {
            source: 3,
            group: 0,
            value: 1,
        },
    }
}

fn run_direct_engine<E>(engine: &mut E, cell: Cell, phases: &[Phase]) -> Result<Measure>
where
    E: direct::FrontierEngine<
        Plan = direct::Plan,
        Change = direct::Change,
        Output = Vec<i64>,
        Error = direct::EngineError,
    >,
{
    let install_start = Instant::now();
    engine.install(direct_plan(cell.shape))?;
    let mut measured = Measure {
        install: install_start.elapsed(),
        ..Measure::default()
    };
    let mut oracle = Oracle::default();
    for (step, phase) in phases.iter().enumerate() {
        let changes: Vec<_> = phase
            .changes
            .iter()
            .map(|c| direct::Change {
                source: c.table.index() as u8,
                row: direct::SourceRow {
                    id: c.id,
                    cells: vec![c.a, c.b],
                },
                weight: c.sign as i8,
            })
            .collect();
        let start = Instant::now();
        let span = tracing::info_span!("stress_phase", phase = phase.name);
        let entered = span.enter();
        engine.apply(&changes)?;
        drop(entered);
        record_apply(&mut measured, phase.name, start.elapsed());
        oracle.apply(phase)?;
        if step < 2 || step as i64 % cell.check_every == 0 || step + 1 == phases.len() {
            let start = Instant::now();
            let mut got = engine.snapshot()?;
            got.sort();
            measured.read += start.elapsed();
            measured.rows = check(got, oracle.snapshot(cell.shape), "direct-iso", cell, step)?;
        }
    }
    measured.rss = hafley_observe::rusage::sample().peak_rss_bytes;
    Ok(measured)
}

fn run_direct(
    cell: Cell,
    phases: &[Phase],
    sqlite: bool,
    observe: bool,
    file: bool,
) -> Result<Measure> {
    if sqlite {
        let file_db = file.then(TempDb::new);
        let mut engine = if let Some(file_db) = &file_db {
            direct::SqliteEngine::open(&file_db.path)?
        } else {
            direct::SqliteEngine::memory()?
        };
        if file {
            engine
                .connection()
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        }
        let (recorder, _guard) = if observe {
            let (recorder, layer) = hafley_observe::CountRecorder::new();
            let guard = tracing_subscriber::registry().with(layer).set_default();
            hafley_observe::sqlite::instrument(engine.connection());
            (Some(recorder), Some(guard))
        } else {
            (None, None)
        };
        let mut measured = run_direct_engine(&mut engine, cell, phases)?;
        if let Some(recorder) = recorder {
            hafley_observe::sqlite::silence(engine.connection());
            let sums = recorder.event_sums(
                hafley_observe::sqlite::SQLITE_TARGET,
                tracing::Level::DEBUG,
                "stress_phase",
                "phase",
                None,
            );
            let batch = sums.get(&(String::from("batch"), String::new()));
            measured.batch_vm = Some(batch.map_or(0, |s| s.sum_of("vm_step") as u64));
            measured.batch_statements = Some(batch.map_or(0, |s| s.events));
            measured.batch_fullscan = Some(batch.map_or(0, |s| s.sum_of("fullscan_step") as u64));
        }
        measured.sqlite_bytes = hafley_observe::sqlite_memory::sample(engine.connection())
            .map(|memory| memory.allocator_current_bytes);
        if let Some(file_db) = &file_db {
            let (db_bytes, wal_bytes) = file_db.bytes();
            measured.db_bytes = Some(db_bytes);
            measured.wal_bytes = Some(wal_bytes);
        }
        Ok(measured)
    } else {
        run_direct_engine(&mut direct::RustEngine::new(), cell, phases)
    }
}

fn run_rust_iso(cell: Cell, phases: &[Phase]) -> Result<Measure> {
    use rust_iso::FrontierEngine as _;
    let install_start = Instant::now();
    let mut engine = rust_iso::Engine::new();
    let sources = [
        engine.define_relation("membership", 2),
        engine.define_relation("permission", 2),
        engine.define_relation("direct_grant", 2),
        engine.define_relation("job", 3),
    ];
    let plan = if cell.shape == Shape::Access {
        rust_iso::PlanNode::Union {
            inputs: vec![
                rust_iso::PlanNode::Scan {
                    relation: sources[2],
                },
                rust_iso::PlanNode::Project {
                    input: Box::new(rust_iso::PlanNode::Join {
                        left: Box::new(rust_iso::PlanNode::Scan {
                            relation: sources[0],
                        }),
                        right: Box::new(rust_iso::PlanNode::Scan {
                            relation: sources[1],
                        }),
                        on_left: 1,
                        on_right: 0,
                    }),
                    columns: vec![0, 3],
                },
            ],
        }
    } else {
        rust_iso::PlanNode::Aggregate {
            input: Box::new(rust_iso::PlanNode::Scan {
                relation: sources[3],
            }),
            group_by: vec![1],
            count: true,
            sum: Some(2),
        }
    };
    let program = engine
        .install(&rust_iso::Program::one("result", plan))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut measured = Measure {
        install: install_start.elapsed(),
        ..Measure::default()
    };
    let mut oracle = Oracle::default();
    for (step, phase) in phases.iter().enumerate() {
        let inspect = step < 2 || step as i64 % cell.check_every == 0 || step + 1 == phases.len();
        let before = inspect.then(|| oracle.snapshot(cell.shape));
        let changes = phase
            .changes
            .iter()
            .map(|c| rust_iso::SourceChange {
                relation: sources[c.table.index()],
                sign: if c.sign > 0 {
                    rust_iso::Sign::Plus
                } else {
                    rust_iso::Sign::Minus
                },
                row: if c.table == Table::Job {
                    vec![c.id, c.a, c.b]
                } else {
                    vec![c.a, c.b]
                },
            })
            .collect();
        let start = Instant::now();
        let deltas = engine
            .apply(
                program,
                rust_iso::Frontier {
                    id: format!("{step}"),
                    changes,
                },
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        record_apply(&mut measured, phase.name, start.elapsed());
        oracle.apply(phase)?;
        if inspect {
            let expected = oracle.snapshot(cell.shape);
            let mut got_delta: Vec<_> = deltas
                .into_iter()
                .flat_map(|output| output.changes)
                .map(|change| {
                    std::iter::once(if change.sign == rust_iso::Sign::Plus {
                        1
                    } else {
                        -1
                    })
                    .chain(change.row)
                    .collect::<Vec<_>>()
                })
                .collect();
            got_delta.sort();
            let wanted = expected_delta(before.expect("checked frontier"), &expected);
            if got_delta != wanted {
                bail!("rust-iso signed delta mismatch at step {step}: got {got_delta:?}, expected {wanted:?}");
            }
            let start = Instant::now();
            let mut got = engine
                .snapshot(program, "result")
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            got.sort();
            measured.read += start.elapsed();
            measured.rows = check(got, expected, "rust-iso", cell, step)?;
        }
    }
    measured.rss = hafley_observe::rusage::sample().peak_rss_bytes;
    Ok(measured)
}

fn run_dd(cell: Cell, phases: &[Phase]) -> Result<Measure> {
    let install_start = Instant::now();
    let engine = dd_packet::Engine::new(if cell.shape == Shape::Access {
        dd_packet::Shape::Access
    } else {
        dd_packet::Shape::Group
    });
    let mut measured = Measure {
        install: install_start.elapsed(),
        ..Measure::default()
    };
    let mut oracle = Oracle::default();
    for (step, phase) in phases.iter().enumerate() {
        let inspect = step < 2 || step as i64 % cell.check_every == 0 || step + 1 == phases.len();
        let before = inspect.then(|| oracle.snapshot(cell.shape));
        let changes = phase
            .changes
            .iter()
            .map(|change| dd_packet::Change {
                table: change.table.index(),
                id: change.id,
                a: change.a,
                b: change.b,
                weight: change.sign as isize,
            })
            .collect();
        let start = Instant::now();
        let deltas = engine.apply(changes).map_err(anyhow::Error::msg)?;
        record_apply(&mut measured, phase.name, start.elapsed());
        oracle.apply(phase)?;
        if inspect {
            let expected = oracle.snapshot(cell.shape);
            let mut got_delta: Vec<_> = deltas
                .into_iter()
                .map(|(row, weight)| {
                    std::iter::once(weight as i64)
                        .chain(row)
                        .collect::<Vec<_>>()
                })
                .collect();
            got_delta.sort();
            let wanted = expected_delta(before.expect("checked frontier"), &expected);
            if got_delta != wanted {
                bail!("dd signed delta mismatch at step {step}: got {got_delta:?}, expected {wanted:?}");
            }
            let start = Instant::now();
            let got = engine.snapshot().map_err(anyhow::Error::msg)?;
            measured.read += start.elapsed();
            measured.rows = check(got, expected, "dd", cell, step)?;
        }
    }
    measured.rss = hafley_observe::rusage::sample().peak_rss_bytes;
    Ok(measured)
}

fn numbers(value: &str) -> Result<Vec<i64>> {
    value
        .split(',')
        .map(|part| part.parse().with_context(|| format!("bad integer {part}")))
        .collect()
}

fn main() -> Result<()> {
    let mut shapes = vec![Shape::Access, Shape::Group];
    let mut rows = vec![32];
    let mut batches = vec![8];
    let mut fanouts = vec![1];
    let mut groups = vec![true, false];
    let mut churn = 0;
    let mut check_every = 1;
    let mut reps = 1usize;
    let mut rep = 0usize;
    let mut observe = false;
    let mut file = false;
    let mut max_batch_vm = None::<u64>;
    let mut max_batch_fullscan = None::<u64>;
    let mut arms = [
        "direct-rust",
        "direct-sqlite",
        "rust-iso",
        "sqlite-iso",
        "sqlite-iso-ext",
        "sqlite-ivm-frontier",
        "dd-ext",
        "sqlite-ivm",
        "dd",
    ]
    .map(str::to_string)
    .to_vec();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let child = args.iter().any(|arg| arg == "--child");
    for arg in args {
        if arg == "--child" {
            continue;
        }
        let (key, value) = arg.split_once('=').context("use --key=value")?;
        match key {
            "--shapes" => {
                shapes = value
                    .split(',')
                    .map(|s| match s {
                        "access" => Ok(Shape::Access),
                        "group" => Ok(Shape::Group),
                        _ => bail!("bad shape {s}"),
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            "--rows" => rows = numbers(value)?,
            "--batch" => batches = numbers(value)?,
            "--fanout" => fanouts = numbers(value)?,
            "--groups" => {
                groups = value
                    .split(',')
                    .map(|s| match s {
                        "hot" => Ok(true),
                        "spread" => Ok(false),
                        _ => bail!("bad group distribution {s}"),
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            "--churn" => churn = value.parse()?,
            "--check-every" => check_every = value.parse()?,
            "--reps" => reps = value.parse()?,
            "--rep" => rep = value.parse()?,
            "--observe" => {
                observe = match value {
                    "on" => true,
                    "off" => false,
                    _ => bail!("--observe=on|off"),
                }
            }
            "--storage" => {
                file = match value {
                    "memory" => false,
                    "file" => true,
                    _ => bail!("--storage=memory|file"),
                }
            }
            "--max-batch-vm" => max_batch_vm = Some(value.parse()?),
            "--max-batch-fullscan" => max_batch_fullscan = Some(value.parse()?),
            "--arms" => arms = value.split(',').map(str::to_string).collect(),
            _ => bail!("unknown flag {key}"),
        }
    }
    if rows.is_empty()
        || batches.is_empty()
        || fanouts.is_empty()
        || groups.is_empty()
        || arms.is_empty()
        || reps == 0
        || rows.iter().any(|&v| v < 1)
        || batches.iter().any(|&v| v < 1)
        || fanouts.iter().any(|&v| v < 1)
        || churn < 0
        || check_every < 1
    {
        bail!("nonempty shapes/arms and positive rows, batch, fanout, reps, check-every required; churn must be nonnegative");
    }
    if (max_batch_vm.is_some() || max_batch_fullscan.is_some()) && !observe {
        bail!("VM/fullscan budgets require --observe=on");
    }
    for arm in &arms {
        if ![
            "direct-rust",
            "direct-sqlite",
            "rust-iso",
            "sqlite-iso",
            "sqlite-iso-ext",
            "sqlite-ivm-frontier",
            "dd-ext",
            "sqlite-ivm",
            "dd",
        ]
        .contains(&arm.as_str())
        {
            bail!("bad arm {arm}");
        }
    }
    if !child {
        println!("shape\trows\tbatch\tfanout\tgroups\tchurn\tstorage\trep\tarm\tinstall_ms\tseed_ms\tbatch_ms\tchurn_ms\tread_ms\tresult_rows\tsqlite_allocator_bytes\tprocess_peak_rss_bytes\tdb_bytes\twal_bytes\tbatch_vm_steps\tbatch_statements\tbatch_fullscan_steps");
    }
    for &shape in &shapes {
        for &n in &rows {
            for &batch in &batches {
                for &fanout in &fanouts {
                    for &hot in if shape == Shape::Access {
                        &groups[..1]
                    } else {
                        &groups[..]
                    } {
                        let cell = Cell {
                            shape,
                            rows: n,
                            batch,
                            fanout,
                            hot,
                            churn,
                            check_every,
                        };
                        let phases = fixture(cell);
                        for arm in &arms {
                            if !child {
                                for current_rep in 0..reps {
                                    let mut child_args = vec![
                                        "--child".to_string(),
                                        format!(
                                            "--shapes={}",
                                            if shape == Shape::Access {
                                                "access"
                                            } else {
                                                "group"
                                            }
                                        ),
                                        format!("--rows={n}"),
                                        format!("--batch={batch}"),
                                        format!("--fanout={fanout}"),
                                        format!("--groups={}", if hot { "hot" } else { "spread" }),
                                        format!("--churn={churn}"),
                                        format!("--check-every={check_every}"),
                                        format!("--observe={}", if observe { "on" } else { "off" }),
                                        format!(
                                            "--storage={}",
                                            if file { "file" } else { "memory" }
                                        ),
                                        format!("--rep={current_rep}"),
                                        format!("--arms={arm}"),
                                    ];
                                    if let Some(maximum) = max_batch_vm {
                                        child_args.push(format!("--max-batch-vm={maximum}"));
                                    }
                                    if let Some(maximum) = max_batch_fullscan {
                                        child_args.push(format!("--max-batch-fullscan={maximum}"));
                                    }
                                    let output = Command::new(std::env::current_exe()?)
                                        .args(child_args)
                                        .output()?;
                                    if !output.status.success() {
                                        bail!(
                                            "{arm} failed for {cell:?}: {} {}",
                                            String::from_utf8_lossy(&output.stdout),
                                            String::from_utf8_lossy(&output.stderr)
                                        );
                                    }
                                    print!("{}", String::from_utf8(output.stdout)?);
                                }
                                continue;
                            }
                            let result = match arm.as_str() {
                                "direct-rust" => run_direct(cell, &phases, false, observe, file),
                                "direct-sqlite" => run_direct(cell, &phases, true, observe, file),
                                "rust-iso" => run_rust_iso(cell, &phases),
                                "sqlite-iso" => {
                                    run_sqlite(cell, &phases, SqliteMode::IsoRust, observe, file)
                                }
                                "sqlite-iso-ext" => run_sqlite(
                                    cell,
                                    &phases,
                                    SqliteMode::IsoExtension,
                                    observe,
                                    file,
                                ),
                                "dd-ext" => run_sqlite(
                                    cell,
                                    &phases,
                                    SqliteMode::DdExtension,
                                    observe,
                                    file,
                                ),
                                "sqlite-ivm-frontier" => run_sqlite(
                                    cell,
                                    &phases,
                                    SqliteMode::MainFrontier,
                                    observe,
                                    file,
                                ),
                                "sqlite-ivm" => {
                                    run_sqlite(cell, &phases, SqliteMode::Production, observe, file)
                                }
                                "dd" => run_dd(cell, &phases),
                                _ => unreachable!(),
                            }?;
                            let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
                            println!("{}\t{n}\t{batch}\t{fanout}\t{}\t{churn}\t{}\t{rep}\t{arm}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", if shape == Shape::Access { "access" } else { "group" }, if hot { "hot" } else { "spread" }, if file { "file" } else { "memory" }, ms(result.install), ms(result.seed), ms(result.batch), ms(result.churn), ms(result.read), result.rows, result.sqlite_bytes.map_or(String::new(), |v| v.to_string()), result.rss.map_or(String::new(), |v| v.to_string()), result.db_bytes.map_or(String::new(), |v| v.to_string()), result.wal_bytes.map_or(String::new(), |v| v.to_string()), result.batch_vm.map_or(String::new(), |v| v.to_string()), result.batch_statements.map_or(String::new(), |v| v.to_string()), result.batch_fullscan.map_or(String::new(), |v| v.to_string()));
                            if let Some(maximum) = max_batch_vm {
                                if let Some(actual) = result.batch_vm {
                                    if actual > maximum {
                                        bail!("{arm} batch VM budget exceeded: {actual} > {maximum} at {cell:?}");
                                    }
                                }
                            }
                            if let Some(maximum) = max_batch_fullscan {
                                if let Some(actual) = result.batch_fullscan {
                                    if actual > maximum {
                                        bail!("{arm} batch fullscan budget exceeded: {actual} > {maximum} at {cell:?}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_arm_agrees_on_join_cross_term_and_group_churn() -> Result<()> {
        for (shape, hot) in [
            (Shape::Access, true),
            (Shape::Group, true),
            (Shape::Group, false),
        ] {
            let cell = Cell {
                shape,
                rows: 8,
                batch: 2,
                fanout: 2,
                hot,
                churn: 2,
                check_every: 1,
            };
            let phases = fixture(cell);
            run_direct(cell, &phases, false, false, false)?;
            run_direct(cell, &phases, true, false, false)?;
            run_rust_iso(cell, &phases)?;
            run_sqlite(cell, &phases, SqliteMode::IsoRust, false, false)?;
            run_sqlite(cell, &phases, SqliteMode::Production, false, false)?;
            run_dd(cell, &phases)?;
        }
        Ok(())
    }

    #[test]
    fn indexed_sqlite_batch_work_stays_flat_as_unrelated_state_grows() -> Result<()> {
        let run = |rows, iso| {
            let cell = Cell {
                shape: Shape::Access,
                rows,
                batch: 8,
                fanout: 1,
                hot: true,
                churn: 0,
                check_every: 1,
            };
            run_sqlite(
                cell,
                &fixture(cell),
                if iso {
                    SqliteMode::IsoRust
                } else {
                    SqliteMode::Production
                },
                true,
                false,
            )
        };
        for iso in [true, false] {
            let small = run(32, iso)?;
            let large = run(256, iso)?;
            assert_eq!(small.batch_vm, large.batch_vm, "VM steps for iso={iso}");
            assert_eq!(
                small.batch_statements, large.batch_statements,
                "statements for iso={iso}"
            );
            assert_eq!(
                small.batch_fullscan, large.batch_fullscan,
                "fullscan steps for iso={iso}"
            );
        }
        Ok(())
    }

    #[test]
    fn file_mode_reports_database_and_wal_pressure() -> Result<()> {
        let cell = Cell {
            shape: Shape::Group,
            rows: 8,
            batch: 2,
            fanout: 1,
            hot: true,
            churn: 1,
            check_every: 1,
        };
        for iso in [true, false] {
            let result = run_sqlite(
                cell,
                &fixture(cell),
                if iso {
                    SqliteMode::IsoRust
                } else {
                    SqliteMode::Production
                },
                false,
                true,
            )?;
            assert!(result.db_bytes.unwrap_or_default() > 0);
            assert!(result.wal_bytes.unwrap_or_default() > 0);
        }
        Ok(())
    }
}
