//! Scale sweep across arms.
//!
//! Per circuit, per n, per fanout, per arm: wall ms, the four write-phase
//! means, recompute ms, peak RSS growth, disk bytes read and written, db size
//! on disk, arrangement rows, and the derived `ivm/dd` wall ratio. Writes WAL.

use crate::fixture::{Write, WriteRow};
use crate::oracle::{sort_rows, Cell};
use crate::scale_dd::DdScale;
use anyhow::{bail, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::info;

pub const ARMS: [&str; 3] = ["sqlite-ivm", "sqlite-query", "dd"];

const SCALE_QUERIES: [(&str, &str); 6] = [
    ("chain", "SELECT a.k AS c0,c.v AS c1 FROM a JOIN b ON a.v=b.k JOIN c ON b.v=c.k"),
    ("join", "SELECT a.k AS c0,a.v*b.v AS c1 FROM a JOIN b ON a.k=b.k"),
    ("group", "SELECT k AS c0,count(*) AS c1,sum(v) AS c2 FROM a GROUP BY k"),
    ("distinct", "SELECT DISTINCT v AS c0 FROM a"),
    (
        "window",
        "SELECT k AS c0,v AS c1,ROW_NUMBER() OVER (PARTITION BY k ORDER BY v,id) AS c2 FROM a",
    ),
    (
        "reach",
        "WITH RECURSIVE reachable(node) AS (SELECT k FROM b UNION SELECT a.v FROM a JOIN reachable r ON a.k=r.node) SELECT node AS c0 FROM reachable",
    ),
];

const SCALE_PRAGMAS: &str = "PRAGMA journal_mode=WAL;PRAGMA synchronous=NORMAL;\
                             PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;";

pub struct Scale {
    pub circuits: Vec<String>,
    pub ns: Vec<i64>,
    pub fanouts: Vec<i64>,
    pub arms: Vec<String>,
    pub reps: usize,
    pub out: PathBuf,
}

/// One mutation, rendered as DELETE+INSERT so every arm sees the same stream.
struct Phase {
    writes: Vec<Write>,
    one_txn: bool,
}

struct CellSpec {
    circuit: String,
    query: String,
    n: i64,
    fanout: i64,
    groups: i64,
}

impl CellSpec {
    fn new(circuit: &str, query: &str, n: i64, fanout: i64) -> Self {
        Self {
            circuit: circuit.to_string(),
            query: query.to_string(),
            n,
            fanout,
            groups: n / fanout + 1,
        }
    }

    fn seed_row(&self, id: i64) -> [i64; 3] {
        [id, (id * 7) % self.groups, (id * 13) % self.groups]
    }

    fn seed(&self) -> [Vec<[i64; 3]>; 3] {
        std::array::from_fn(|_| (0..self.n).map(|id| self.seed_row(id)).collect())
    }

    fn phases(&self) -> Vec<Phase> {
        let row = |id: i64, k: i64, v: i64| Write {
            table: "a",
            id,
            row: Some(WriteRow(id, Cell::Int(k), Cell::Int(v))),
        };
        let insert = (0..40)
            .map(|i| {
                let id = self.n + i;
                let [_, k, v] = self.seed_row(id);
                row(id, k, v)
            })
            .collect();
        let delete = (0..40)
            .map(|i| Write { table: "a", id: self.n + i, row: None })
            .collect();
        let update = (0..40)
            .map(|id| {
                let [_, k, v] = self.seed_row(id);
                row(id, k, (v + 1) % self.groups)
            })
            .collect();
        let replace = (0..1000.min(self.n))
            .map(|id| {
                let [_, k, v] = self.seed_row(id);
                row(id, k, v)
            })
            .collect();
        vec![
            Phase { writes: insert, one_txn: false },
            Phase { writes: delete, one_txn: false },
            Phase { writes: update, one_txn: false },
            Phase { writes: replace, one_txn: true },
        ]
    }
}

fn write_sql(write: &Write) -> String {
    match &write.row {
        None => format!("DELETE FROM {} WHERE id={};", write.table, write.id),
        Some(row) => format!(
            "DELETE FROM {} WHERE id={};INSERT INTO {}(id,k,v) VALUES({},{},{});",
            write.table,
            write.id,
            write.table,
            row.0,
            row.1.as_int().unwrap_or(0),
            row.2.as_int().unwrap_or(0),
        ),
    }
}

fn read_reps(n: i64) -> usize {
    if n >= 100_000 {
        1
    } else if n >= 10_000 {
        3
    } else {
        20
    }
}

trait Arm {
    fn setup(&mut self, spec: &CellSpec) -> Result<()>;
    fn apply(&mut self, phase: &Phase) -> Result<Duration>;
    fn read(&mut self) -> Result<Vec<Vec<Cell>>>;
    /// `(db bytes on disk, arrangement rows)`.
    fn metrics(&self) -> Result<(u64, i64)>;
    fn teardown(&mut self) -> Result<()>;
}

#[derive(PartialEq, Eq)]
enum SqlKind {
    Ivm,
    Query,
}

struct SqliteScale {
    kind: SqlKind,
    path: PathBuf,
    query: String,
    db: Option<Connection>,
}

impl SqliteScale {
    fn new(kind: SqlKind, path: PathBuf) -> Self {
        Self { kind, path, query: String::new(), db: None }
    }
}

impl Arm for SqliteScale {
    fn setup(&mut self, spec: &CellSpec) -> Result<()> {
        crate::arms::remove_db_files(&self.path);
        let db = Connection::open(&self.path)?;
        db.execute_batch(SCALE_PRAGMAS)?;
        db.execute_batch(crate::arms::source_ddl())?;
        if self.kind == SqlKind::Ivm {
            sqlite_ivm::extension::register(&db)?;
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE v USING sqlite_ivm('{}')",
                spec.query.replace('\'', "''")
            ))?;
        }
        self.query = spec.query.clone();
        db.execute_batch("BEGIN;")?;
        for table in ["a", "b", "c"] {
            let mut insert = db.prepare(&format!(
                "INSERT INTO {table}(id,k,v) VALUES(?1,?2,?3)"
            ))?;
            for id in 0..spec.n {
                let [id, k, v] = spec.seed_row(id);
                insert.execute((id, k, v))?;
            }
        }
        db.execute_batch("COMMIT;")?;
        self.db = Some(db);
        Ok(())
    }

    fn apply(&mut self, phase: &Phase) -> Result<Duration> {
        let db = self.db.as_ref().expect("setup before apply");
        let started = Instant::now();
        if phase.one_txn {
            db.execute_batch("BEGIN;")?;
        }
        for write in &phase.writes {
            db.execute_batch(&write_sql(write))?;
        }
        if phase.one_txn {
            db.execute_batch("COMMIT;")?;
        }
        Ok(started.elapsed())
    }

    fn read(&mut self) -> Result<Vec<Vec<Cell>>> {
        let db = self.db.as_ref().expect("setup before read");
        let sql = if self.kind == SqlKind::Ivm { "SELECT * FROM v" } else { &self.query };
        crate::arms::read_rows(db, sql)
    }

    fn metrics(&self) -> Result<(u64, i64)> {
        let db = self.db.as_ref().expect("setup before metrics");
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let db_bytes = std::fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        let arrangement = if self.kind == SqlKind::Ivm {
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE '__ivm\\_v\\_%' ESCAPE '\\'",
                [],
                |row| row.get(0),
            )?
        } else {
            0
        };
        Ok((db_bytes, arrangement))
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(db) = self.db.take() {
            let _ = db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
        crate::arms::remove_db_files(&self.path);
        Ok(())
    }
}

struct DdArm {
    inner: Option<DdScale>,
}

impl DdArm {
    fn new() -> Self {
        Self { inner: None }
    }
}

impl Arm for DdArm {
    fn setup(&mut self, spec: &CellSpec) -> Result<()> {
        let dd = DdScale::start(&spec.circuit)?;
        dd.seed(spec.seed())?;
        self.inner = Some(dd);
        Ok(())
    }

    fn apply(&mut self, phase: &Phase) -> Result<Duration> {
        self.inner.as_ref().expect("setup before apply").apply(&phase.writes, phase.one_txn)
    }

    fn read(&mut self) -> Result<Vec<Vec<Cell>>> {
        let rows = self.inner.as_ref().expect("setup before read").read()?;
        Ok(rows.into_iter().map(|row| row.into_iter().map(Cell::Int).collect()).collect())
    }

    fn metrics(&self) -> Result<(u64, i64)> {
        Ok((0, 0))
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(mut dd) = self.inner.take() {
            dd.teardown()?;
        }
        Ok(())
    }
}

/// The oracle is a plain in-memory SQLite that takes the same seed and writes
/// and answers the circuit query directly; every arm's final read must match.
fn oracle_rows(spec: &CellSpec) -> Result<Vec<Vec<Cell>>> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(crate::arms::source_ddl())?;
    for table in ["a", "b", "c"] {
        let mut insert =
            db.prepare(&format!("INSERT INTO {table}(id,k,v) VALUES(?1,?2,?3)"))?;
        for id in 0..spec.n {
            let [id, k, v] = spec.seed_row(id);
            insert.execute((id, k, v))?;
        }
    }
    for phase in spec.phases() {
        if phase.one_txn {
            db.execute_batch("BEGIN;")?;
        }
        for write in &phase.writes {
            db.execute_batch(&write_sql(write))?;
        }
        if phase.one_txn {
            db.execute_batch("COMMIT;")?;
        }
    }
    Ok(sort_rows(crate::arms::read_rows(&db, &spec.query)?))
}

#[derive(Serialize, Clone)]
struct ScaleRow {
    circuit: String,
    fanout: i64,
    n: i64,
    rep: usize,
    arm: String,
    wall_ms: f64,
    insert_ms: f64,
    delete_ms: f64,
    update_ms: f64,
    replace_ms: f64,
    recompute_ms: f64,
    peak_rss_mib: f64,
    disk_read_bytes: Option<u64>,
    disk_write_bytes: Option<u64>,
    db_bytes: u64,
    arrangement_rows: i64,
    ivm_over_dd: Option<f64>,
}

impl Scale {
    pub fn run(&self) -> Result<u8> {
        std::fs::create_dir_all(&self.out)?;
        let mut rows: Vec<ScaleRow> = Vec::new();
        let mut exit = 0u8;
        let mut tsv = std::fs::File::create(self.out.join("scale.tsv"))?;
        writeln!(
            tsv,
            "circuit\tfanout\tn\trep\tarm\twall_ms\tinsert_ms\tdelete_ms\tupdate_ms\treplace_ms\
             \trecompute_ms\tpeak_rss_mib\tdisk_read_bytes\tdisk_write_bytes\tdb_bytes\
             \tarrangement_rows\tivm_over_dd"
        )?;
        tsv.flush()?;
        for circuit in &self.circuits {
            let Some((_, query)) = SCALE_QUERIES.iter().find(|(name, _)| name == circuit) else {
                bail!("unknown scale circuit {circuit}");
            };
            for &fanout in &self.fanouts {
                for &n in &self.ns {
                    let spec = CellSpec::new(circuit, query, n, fanout);
                    let started = Instant::now();
                    for rep in 0..self.reps.max(1) {
                        match self.cell(&spec, rep) {
                            Ok(cell_rows) => {
                                for row in &cell_rows {
                                    for (column, ms) in [
                                        ("wall", row.wall_ms),
                                        ("insert", row.insert_ms),
                                        ("delete", row.delete_ms),
                                        ("update", row.update_ms),
                                        ("replace", row.replace_ms),
                                        ("recompute", row.recompute_ms),
                                    ] {
                                        if ms > 10_000.0 {
                                            println!(
                                                "defect: {} fanout={} n={n} rep={rep} arm={}: {column} {:.1} ms > 10 s",
                                                circuit, fanout, row.arm, ms,
                                            );
                                        }
                                    }
                                    write!(tsv, "{}", row.tsv_line())?;
                                }
                                tsv.flush()?;
                                rows.extend(cell_rows);
                            }
                            Err(error) => {
                                println!(
                                    "defect: {circuit} fanout={fanout} n={n} rep={rep}: {error:#}"
                                );
                                tsv.flush()?;
                                exit = 2;
                            }
                        }
                    }
                    info!(
                        "{} fanout={fanout} n={n} {} reps done in {:.1} s",
                        circuit,
                        self.reps.max(1),
                        started.elapsed().as_secs_f64()
                    );
                }
            }
        }
        render_svgs(&self.out, &rows)?;
        Ok(exit)
    }

    fn cell(&self, spec: &CellSpec, rep: usize) -> Result<Vec<ScaleRow>> {
        let oracle = oracle_rows(spec)?;
        let mut cell_rows: Vec<ScaleRow> = Vec::new();
        for arm_name in &self.arms {
            let scratch = self.out.join("scratch").join(format!(
                "{}-{}-f{}-n{}-r{rep}",
                arm_name, spec.circuit, spec.fanout, spec.n
            ));
            std::fs::create_dir_all(&scratch)?;
            let mut arm: Box<dyn Arm> = match arm_name.as_str() {
                "sqlite-ivm" => {
                    Box::new(SqliteScale::new(SqlKind::Ivm, scratch.join("scale.db")))
                }
                "sqlite-query" => {
                    Box::new(SqliteScale::new(SqlKind::Query, scratch.join("scale.db")))
                }
                "dd" => Box::new(DdArm::new()),
                other => bail!("unknown arm {other}; expected one of {:?}", ARMS),
            };
            let rss_before = crate::arms::peak_rss_bytes();
            let io_before = crate::arms::disk_io_bytes();
            arm.setup(spec)?;
            let phases = spec.phases();
            let mut means = Vec::with_capacity(phases.len());
            let mut wall_ms = 0.0;
            for phase in &phases {
                let ms = arm.apply(phase)?.as_secs_f64() * 1000.0;
                wall_ms += ms;
                means.push(if phase.one_txn { ms } else { ms / phase.writes.len() as f64 });
            }
            let reps = read_reps(spec.n);
            let mut read_total = 0.0;
            for _ in 0..reps {
                let started = Instant::now();
                let actual = sort_rows(arm.read()?);
                read_total += started.elapsed().as_secs_f64() * 1000.0;
                if actual != oracle {
                    bail!(
                        "{arm_name} {} fanout={} n={}: view diverged from recompute",
                        spec.circuit,
                        spec.fanout,
                        spec.n
                    );
                }
            }
            wall_ms += read_total;
            let (db_bytes, arrangement_rows) = arm.metrics()?;
            arm.teardown()?;
            let [insert_ms, delete_ms, update_ms, replace_ms] = means[..] else {
                unreachable!("four write phases")
            };
            let rss = crate::arms::peak_rss_bytes()
                .zip(rss_before)
                .map(|(after, before)| after.saturating_sub(before))
                .unwrap_or(0);
            let io = crate::arms::disk_io_bytes()
                .zip(io_before)
                .map(|(after, before)| {
                    (after.0.saturating_sub(before.0), after.1.saturating_sub(before.1))
                });
            cell_rows.push(ScaleRow {
                circuit: spec.circuit.clone(),
                fanout: spec.fanout,
                n: spec.n,
                rep,
                arm: arm_name.clone(),
                wall_ms,
                insert_ms,
                delete_ms,
                update_ms,
                replace_ms,
                recompute_ms: read_total / reps as f64,
                peak_rss_mib: rss as f64 / 1048576.0,
                disk_read_bytes: io.map(|bytes| bytes.0),
                disk_write_bytes: io.map(|bytes| bytes.1),
                db_bytes,
                arrangement_rows,
                ivm_over_dd: None,
            });
        }
        let ivm = cell_rows.iter().find(|row| row.arm == "sqlite-ivm").map(|row| row.wall_ms);
        let dd = cell_rows.iter().find(|row| row.arm == "dd").map(|row| row.wall_ms);
        if let (Some(ivm), Some(dd)) = (ivm, dd) {
            if dd > 0.0 {
                if let Some(row) = cell_rows.iter_mut().find(|row| row.arm == "sqlite-ivm") {
                    row.ivm_over_dd = Some(ivm / dd);
                }
            }
        }
        Ok(cell_rows)
    }
}

impl ScaleRow {
    /// One TSV record, no trailing newline.
    fn tsv_line(&self) -> String {
        let bytes = |value: Option<u64>| {
            value.map(|bytes| bytes.to_string()).unwrap_or_else(|| "null".to_string())
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}\t{}\n",
            self.circuit,
            self.fanout,
            self.n,
            self.rep,
            self.arm,
            self.wall_ms,
            self.insert_ms,
            self.delete_ms,
            self.update_ms,
            self.replace_ms,
            self.recompute_ms,
            self.peak_rss_mib,
            bytes(self.disk_read_bytes),
            bytes(self.disk_write_bytes),
            self.db_bytes,
            self.arrangement_rows,
            self.ivm_over_dd.map(|ratio| format!("{ratio:.3}")).unwrap_or_default(),
        )
    }
}

fn render_svgs(out: &Path, rows: &[ScaleRow]) -> Result<()> {
    let available = std::process::Command::new("gnuplot")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !available {
        info!("gnuplot not found; skipping SVG render");
        return Ok(());
    }
    let mut circuits: Vec<String> = Vec::new();
    for row in rows {
        if !circuits.contains(&row.circuit) {
            circuits.push(row.circuit.clone());
        }
    }
    for circuit in &circuits {
        let cells: Vec<&ScaleRow> =
            rows.iter().filter(|row| &row.circuit == circuit).collect();
        let median = |mut values: Vec<f64>| -> f64 {
            values.sort_by(|a, b| a.partial_cmp(b).unwrap());
            values[values.len() / 2]
        };
        let mut series: Vec<(String, Vec<(i64, f64)>)> = Vec::new();
        for arm in ARMS {
            for fanout in [1i64, 10] {
                let mut ns: Vec<i64> = cells
                    .iter()
                    .filter(|row| row.arm == arm && row.fanout == fanout)
                    .map(|row| row.n)
                    .collect();
                ns.sort();
                ns.dedup();
                let points: Vec<(i64, f64)> = ns
                    .into_iter()
                    .map(|n| {
                        let values = cells
                            .iter()
                            .filter(|row| row.arm == arm && row.fanout == fanout && row.n == n)
                            .map(|row| row.wall_ms)
                            .collect();
                        (n, median(values))
                    })
                    .collect();
                if !points.is_empty() {
                    series.push((format!("{arm} f={fanout}"), points));
                }
            }
        }
        plot(
            out,
            &format!("scale-{circuit}.svg"),
            "wall ms",
            &series,
            cells.iter().map(|row| row.n),
        )?;
        let ratio: Vec<(String, Vec<(i64, f64)>)> = [1i64, 10]
            .into_iter()
            .filter_map(|fanout| {
                let mut ns: Vec<i64> = cells
                    .iter()
                    .filter(|row| row.fanout == fanout && row.ivm_over_dd.is_some())
                    .map(|row| row.n)
                    .collect();
                ns.sort();
                ns.dedup();
                let points: Vec<(i64, f64)> = ns
                    .into_iter()
                    .map(|n| {
                        let values = cells
                            .iter()
                            .filter(|row| {
                                row.fanout == fanout
                                    && row.n == n
                                    && row.ivm_over_dd.is_some()
                            })
                            .map(|row| row.ivm_over_dd.unwrap())
                            .collect();
                        (n, median(values))
                    })
                    .collect();
                (!points.is_empty()).then(|| (format!("ivm/dd f={fanout}"), points))
            })
            .collect();
        if !ratio.is_empty() {
            plot(
                out,
                &format!("scale-{circuit}-ivm-over-dd.svg"),
                "ivm/dd",
                &ratio,
                cells.iter().map(|row| row.n),
            )?;
        }
    }
    Ok(())
}

fn plot(
    out: &Path,
    name: &str,
    ylabel: &str,
    series: &[(String, Vec<(i64, f64)>)],
    ns: impl Iterator<Item = i64>,
) -> Result<()> {
    let mut ns: Vec<i64> = ns.collect();
    ns.sort();
    ns.dedup();
    let mut script = String::from("set terminal svg size 900,600\nset key outside\n");
    script.push_str("set xlabel 'rows n'\n");
    script.push_str(&format!("set ylabel '{ylabel}'\n"));
    if let (Some(&min), Some(&max)) = (ns.first(), ns.last()) {
        if max / min.max(1) >= 10 {
            script.push_str("set logscale x\n");
        }
    }
    script.push_str(&format!("set output '{}'\n", out.join(name).display()));
    let mut plot = String::new();
    for (index, (label, points)) in series.iter().enumerate() {
        let data: String = points.iter().map(|(n, y)| format!("{n} {y:.3}\n")).collect();
        script.push_str(&format!("$series_{index} << EOD\n{data}EOD\n"));
        if index > 0 {
            plot.push_str(", ");
        }
        plot.push_str(&format!("$series_{index} with linespoints title '{label}'"));
    }
    script.push_str(&format!("plot {plot}\n"));
    let mut child = std::process::Command::new("gnuplot")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    child.stdin.take().expect("piped stdin").write_all(script.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("gnuplot failed for {name}");
    }
    Ok(())
}