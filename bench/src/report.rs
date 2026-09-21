//! Shootout and scale drivers: engine rotation, per-case receipts, the
//! quick-2 report table, and the scale TSV/SVG outputs.

use crate::arms::dd::Dd;
use crate::arms::pg_ivm::{Cluster, PgIvm};
use crate::arms::pg_query::PgQuery;
use crate::arms::sqlite_ivm::SqliteIvm;
use crate::arms::sqlite_query::SqliteQuery;
use crate::arms::{Arm, Setup};
use crate::fixture::{self, Fixture};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::info;


pub const DEFAULT_ENGINES: [&str; 5] = ["sqlite-ivm", "pg-ivm", "sqlite-query", "pg-query", "dd"];
pub struct Shootout {
    pub smoke: bool,
    pub engines: Vec<String>,
    pub out: PathBuf,
    pub circuits: Option<Vec<String>>,
    pub pg_prefix: Option<PathBuf>,
}

pub struct Scale {
    pub circuits: Vec<String>,
    pub ns: Vec<i64>,
    pub fanouts: Vec<i64>,
    pub out: PathBuf,
}

#[derive(Serialize, Clone)]
struct CaseSummary {
    engine: String,
    circuit: String,
    median_ms: f64,
    peak_rss_bytes: Option<u64>,
    disk_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unsupported: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Shootout {
    fn dims(&self) -> (i64, i64, i64) {
        if self.smoke {
            (24, 3, 4)
        } else {
            (400, 10, 10)
        }
    }

    fn reps(&self) -> usize {
        if self.smoke {
            1
        } else {
            3
        }
    }

    fn warmups(&self) -> usize {
        if self.smoke {
            0
        } else {
            1
        }
    }

    fn fixture(&self, circuit: &str) -> Result<Fixture> {
        let (rows, batch, fanout) = self.dims();
        fixture::make_fixture(circuit, rows, batch, fanout, crate::oracle::Domain::Integers)
    }

    fn all_circuits(&self) -> Result<Vec<String>> {
        if let Some(requested) = &self.circuits {
            let known = fixture::circuit_names();
            for circuit in requested {
                if !known.contains(&&**circuit) {
                    bail!("unknown circuit {circuit}");
                }
            }
            Ok(requested.clone())
        } else {
            Ok(fixture::circuit_names().into_iter().map(String::from).collect())
        }
    }

    fn jsonl(&self, value: &impl serde::Serialize) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.out.join("circuits.jsonl"))?;
        writeln!(file, "{}", serde_json::to_string(value)?)?;
        Ok(())
    }

    pub fn run(&self) -> Result<u8> {
        let mut exit = 0u8;
        let mut summaries: Vec<CaseSummary> = Vec::new();
        std::fs::create_dir_all(&self.out)?;
        let circuits = self.all_circuits()?;
        let extension = if self.engines.iter().any(|engine| engine == "sqlite-ivm") {
            Some(SqliteIvm::resolve_extension()?)
        } else {
            None
        };
        let needs_pg = self
            .engines
            .iter()
            .any(|engine| engine == "pg-ivm" || engine == "pg-query");
        let cluster = if needs_pg {
            let prefix = self
                .pg_prefix
                .clone()
                .or_else(|| std::env::var("IVM_POSTGRES_PREFIX").ok().map(PathBuf::from));
            let prefix = prefix.ok_or_else(|| {
                anyhow!("pg engines requested; pass --pg-prefix or set IVM_POSTGRES_PREFIX")
            })?;
            Some(Cluster::start(&prefix, &self.out)?)
        } else {
            None
        };
        for circuit in &circuits {
            for engine in &self.engines {
                let fixture = self.fixture(circuit)?;
                let mut totals: Vec<f64> = Vec::new();
                let mut last_rss: Option<u64> = None;
                let mut last_disk: Option<u64> = None;
                let mut unsupported: Option<String> = None;
                let mut error: Option<String> = None;
                let total_reps = self.warmups() + self.reps();
                for rep in 0..total_reps {
                    let timed = rep >= self.warmups();
                    let scratch =
                        self.out.join("scratch").join(format!("{engine}-{circuit}-{rep}"));
                    std::fs::create_dir_all(&scratch)?;
                    info!("{} {} rep {rep}: case start", engine, circuit);
                    let mut arm: Box<dyn Arm> = match engine.as_str() {
                        "sqlite-ivm" => Box::new(SqliteIvm::new(
                            scratch.join("bench.db"),
                            extension.clone().expect("resolved above"),
                        )),
                        "sqlite-query" => Box::new(SqliteQuery::new(scratch.join("bench.db"))),
                        "pg-ivm" => Box::new(PgIvm::new(cluster.clone().expect("cluster started"))),
                        "pg-query" => {
                            Box::new(PgQuery::new(cluster.clone().expect("cluster started")))
                        }
                        "dd" => Box::new(Dd::new()),
                        other => bail!("unknown engine {other}"),
                    };
                    info!("case engine {} via arm {}", engine, arm.name());
                    let outcome = run_case(&mut arm, &fixture, timed);
                    self.jsonl(&serde_json::json!({
                        "event": "case-total",
                        "engine": engine,
                        "circuit": circuit,
                        "rep": rep,
                        "timed": timed,
                        "total_ms": outcome.as_ref().ok().map(|stats| stats.total_ms),
                        "checksum": outcome.as_ref().ok().map(|stats| stats.checksum.clone()),
                        "unsupported": outcome.as_ref().err().and_then(|f| f.unsupported()),
                        "error": outcome.as_ref().err().map(|f| f.message()),
                    }))?;
                    match outcome {
                        Ok(stats) => {
                            if timed {
                                totals.push(stats.total_ms);
                                last_rss = stats.peak_rss_bytes;
                                last_disk = stats.disk_bytes;
                            }
                        }
                        Err(failure) => match failure {
                            Failure::Unsupported(reason) => unsupported = Some(reason),
                            Failure::Mismatch(reason) => {
                                error = Some(reason);
                                exit = exit.max(1);
                            }
                            Failure::Execution(reason) => {
                                error = Some(reason);
                                exit = 2;
                            }
                        },
                    }
                    if unsupported.is_some() || error.is_some() {
                        break;
                    }
                }
                if unsupported.is_none() && error.is_none() {
                    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
                }
                let summary = CaseSummary {
                    engine: engine.clone(),
                    circuit: circuit.clone(),
                    median_ms: if unsupported.is_some() || error.is_some() || totals.is_empty() {
                        0.0
                    } else {
                        totals[totals.len() / 2]
                    },
                    peak_rss_bytes: last_rss,
                    disk_bytes: last_disk,
                    unsupported,
                    error,
                };
                self.jsonl(&serde_json::json!({
                    "event": "case-summary",
                    "engine": engine,
                    "circuit": circuit,
                    "median_ms": if summary.unsupported.is_some() || summary.error.is_some() { serde_json::Value::Null } else { serde_json::json!(summary.median_ms) },
                    "unsupported": summary.unsupported,
                    "error": summary.error,
                }))?;
                summaries.push(summary);
            }
        }

        write_report(&self.out, &summaries)?;
        Ok(exit)
    }
}


enum Failure {
    /// pg_ivm rejected the view (SQLSTATE 0A000); the report prints `n/a`.
    Unsupported(String),
    /// Fixture checksum or SQL-oracle disagreement; exit code 1.
    Mismatch(String),
    /// Everything else; exit code 2.
    Execution(String),
}

impl Failure {
    fn unsupported(&self) -> Option<String> {
        match self {
            Failure::Unsupported(reason) => Some(reason.clone()),
            _ => None,
        }
    }
    fn message(&self) -> String {
        match self {
            Failure::Unsupported(reason)
            | Failure::Mismatch(reason)
            | Failure::Execution(reason) => reason.clone(),
        }
    }
}

struct RepStats {
    total_ms: f64,
    checksum: String,
    peak_rss_bytes: Option<u64>,
    disk_bytes: Option<u64>,
}

fn run_case(
    arm: &mut Box<dyn Arm>,
    fixture: &Fixture,
    timed: bool,
) -> std::result::Result<RepStats, Failure> {
    match arm.setup(fixture) {
        Ok(Setup::Ready) => {}
        Ok(Setup::Unsupported { reason }) => {
            let _ = arm.teardown();
            return Err(Failure::Unsupported(reason));
        }
        Err(error) => return Err(Failure::Execution(format!("{error:#}"))),
    }
    let mut total_ms = 0.0;
    let mut checksum = String::new();
    for state in &fixture.states {
        match arm.apply(state) {
            Ok(measure) => {
                if timed {
                    total_ms += measure.wall.as_secs_f64() * 1000.0;
                }
                checksum = measure.checksum;
            }
            Err(error) => {
                let text = format!("{error:#}");
                let _ = arm.teardown();
                if text.contains("mismatch") {
                    return Err(Failure::Mismatch(text));
                }
                return Err(Failure::Execution(text));
            }
        }
    }
    let peak_rss_bytes = arm.peak_rss_bytes();
    let disk_bytes = arm.disk_bytes();
    if let Err(error) = arm.teardown() {
        return Err(Failure::Execution(format!("{error:#}")));
    }
    Ok(RepStats { total_ms, checksum, peak_rss_bytes, disk_bytes })
}
fn write_report(out: &Path, summaries: &[CaseSummary]) -> Result<()> {
    let mut circuits: Vec<String> = Vec::new();
    for summary in summaries {
        if !circuits.contains(&summary.circuit) {
            circuits.push(summary.circuit.clone());
        }
    }
    let mut markdown = String::from("# shootout\n\n");
    markdown.push_str(
        "| circuit | sqlite-ivm | pg-ivm | sqlite-query | pg-query | dd | ivm RSS MiB | pg RSS MiB | ivm disk MiB | pg disk MiB |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    );
    for circuit in &circuits {
        let cell = |engine: &str| -> String {
            summaries
                .iter()
                .find(|summary| summary.engine == engine && summary.circuit == *circuit)
                .map(|summary| {
                    if let Some(reason) = &summary.unsupported {
                        format!("n/a ({reason})")
                    } else if let Some(error) = &summary.error {
                        format!("error ({error})")
                    } else {
                        format!("{:.1}", summary.median_ms)
                    }
                })
                .unwrap_or_else(|| "-".to_string())
        };
        let metric = |engine: &str, pick: fn(&CaseSummary) -> Option<u64>| -> String {
            summaries
                .iter()
                .find(|summary| summary.engine == engine && summary.circuit == *circuit)
                .and_then(pick)
                .map(|bytes| format!("{:.1}", bytes as f64 / 1048576.0))
                .unwrap_or_else(|| "-".to_string())
        };
        markdown.push_str(&format!(
            "| {circuit} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            cell("sqlite-ivm"),
            cell("pg-ivm"),
            cell("sqlite-query"),
            cell("pg-query"),
            cell("dd"),
            metric("sqlite-ivm", |s| s.peak_rss_bytes),
            metric("pg-ivm", |s| s.peak_rss_bytes),
            metric("sqlite-ivm", |s| s.disk_bytes),
            metric("pg-ivm", |s| s.disk_bytes),
        ));
    }
    std::fs::write(out.join("report.md"), &markdown)?;
    std::fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "cases": summaries }))?,
    )?;
    print!("{markdown}");
    std::io::stdout().flush()?;
    info!("report written to {}", out.join("report.md").display());
    Ok(())
}

const SCALE_QUERIES: [(&str, &str); 5] = [
    (
        "chain",
        "SELECT a.k AS c0,c.v AS c1 FROM a JOIN b ON a.v=b.k JOIN c ON b.v=c.k",
    ),
    ("group", "SELECT k,count(*) AS n,sum(v) AS s FROM a GROUP BY k"),
    ("distinct", "SELECT DISTINCT v FROM a"),
    ("topk", "SELECT k,v FROM a ORDER BY v DESC,k DESC LIMIT 10"),
    (
        "reach",
        "WITH RECURSIVE reachable(node) AS (SELECT k FROM b UNION SELECT a.v FROM a JOIN reachable r ON a.k=r.node) SELECT node FROM reachable",
    ),
];

#[derive(Serialize, Clone)]
struct ScaleRow {
    circuit: String,
    fanout: i64,
    n: i64,
    insert_ms: f64,
    delete_ms: f64,
    update_ms: f64,
    replace_ms: f64,
    recompute_ms: f64,
    rss_delta_bytes: Option<u64>,
    db_bytes: u64,
    arrangement_rows: i64,
}

impl Scale {
    pub fn run(&self) -> Result<u8> {
        std::fs::create_dir_all(&self.out)?;
        let mut rows: Vec<ScaleRow> = Vec::new();
        let mut exit = 0u8;
        for circuit in &self.circuits {
            let Some((_, query)) = SCALE_QUERIES.iter().find(|(name, _)| name == circuit) else {
                bail!("unknown scale circuit {circuit}");
            };
            for &fanout in &self.fanouts {
                for &n in &self.ns {
                    let started = Instant::now();
                    match scale_cell(circuit, query, n, fanout) {
                        Ok(row) => {
                            let defects = [
                                ("insert", row.insert_ms),
                                ("delete", row.delete_ms),
                                ("update", row.update_ms),
                                ("replace", row.replace_ms),
                                ("recompute", row.recompute_ms),
                            ];
                            for (column, ms) in defects {
                                if ms > 10_000.0 {
                                    println!(
                                        "defect: {} fanout={} n={n}: {column} {:.1} ms > 10 s",
                                        circuit, fanout, ms,
                                    );
                                }
                            }
                            info!(
                                "{} fanout={fanout} n={n} done in {:.1} s",
                                circuit,
                                started.elapsed().as_secs_f64()
                            );
                            rows.push(row);
                        }
                        Err(error) => {
                            println!("defect: {circuit} fanout={fanout} n={n}: {error:#}");
                            exit = 2;
                        }
                    }
                }
            }
        }
        let mut tsv = String::from(
            "circuit\tfanout\tn\tinsert_ms\tdelete_ms\tupdate_ms\treplace_ms\trecompute_ms\trss_delta_mib\tdb_bytes\tarrangement_rows\n",
        );
        for row in &rows {
            tsv.push_str(&format!(
                "{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}\t{}\t{}\n",
                row.circuit,
                row.fanout,
                row.n,
                row.insert_ms,
                row.delete_ms,
                row.update_ms,
                row.replace_ms,
                row.recompute_ms,
                row.rss_delta_bytes
                    .map(|bytes| format!("{:.1}", bytes as f64 / 1048576.0))
                    .unwrap_or_else(|| "null".to_string()),
                row.db_bytes,
                row.arrangement_rows,
            ));
        }
        std::fs::write(self.out.join("scale.tsv"), &tsv)?;
        print!("{tsv}");
        std::io::stdout().flush()?;
        render_svgs(&self.out, &rows)?;
        Ok(exit)
    }
}

fn scale_cell(circuit: &str, query: &str, n: i64, fanout: i64) -> Result<ScaleRow> {
    use rusqlite::Connection;

    let scratch = std::env::temp_dir().join(format!(
        "ivm-bench-scale-{}-{}-{}-{}",
        circuit,
        fanout,
        n,
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch)?;
    let db_path = scratch.join("scale.db");
    crate::arms::remove_db_files(&db_path);
    let rss_before = crate::arms::peak_rss_bytes();
    let db = Connection::open(&db_path)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;PRAGMA synchronous=NORMAL;PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;",
    )?;
    sqlite_ivm::extension::register(&db)?;
    db.execute_batch(crate::arms::source_ddl())?;
    let groups = n / fanout + 1;
    let seed_row = |id: i64| [id, (id * 7) % groups, (id * 13) % groups];
    let mut seed_sql = String::from("BEGIN;\n");
    for table in ["a", "b", "c"] {
        for id in 0..n {
            let [id, k, v] = seed_row(id);
            seed_sql.push_str(&format!("INSERT INTO {table}(id,k,v) VALUES({id},{k},{v});\n"));
        }
    }
    seed_sql.push_str("COMMIT;\n");
    db.execute_batch(&seed_sql)?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE v USING sqlite_ivm('{}')",
        query.replace('\'', "''")
    ))?;

    // Untimed equivalence check: view output equals plain recompute.
    let query_rows = crate::oracle::sort_rows(crate::arms::read_rows(&db, query)?);
    let view_rows = crate::oracle::sort_rows(crate::arms::read_rows(&db, "SELECT * FROM v")?);
    if view_rows != query_rows {
        bail!("view/recompute mismatch at n={n}");
    }

    let mean_of = |mutations: Vec<String>| -> Result<f64> {
        let mut total = 0.0;
        for sql in &mutations {
            let started = Instant::now();
            db.execute(sql, [])?;
            total += started.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(total / mutations.len() as f64)
    };
    let insert_ms = mean_of(
        (0..40)
            .map(|i| {
                let [id, k, v] = seed_row(n + i);
                format!("INSERT INTO a(id,k,v) VALUES({id},{k},{v});")
            })
            .collect(),
    )?;
    let delete_ms = mean_of(
        (0..40)
            .map(|i| format!("DELETE FROM a WHERE id={};", n + i))
            .collect(),
    )?;
    let update_ms = mean_of(
        (0..40)
            .map(|i| {
                let [_, _, v] = seed_row(i);
                format!("UPDATE a SET v={} WHERE id={i};", (v + 1) % groups)
            })
            .collect(),
    )?;
    let replace_sql = {
        let mut sql = String::from("BEGIN;\n");
        for id in 0..1000.min(n) {
            let [id, k, v] = seed_row(id);
            sql.push_str(&format!(
                "INSERT OR REPLACE INTO a(id,k,v) VALUES({id},{k},{v});\n"
            ));
        }
        sql.push_str("COMMIT;\n");
        sql
    };
    let replace_start = Instant::now();
    db.execute_batch(&replace_sql)?;
    let replace_ms = replace_start.elapsed().as_secs_f64() * 1000.0;

    let reps = if n >= 100_000 {
        1
    } else if n >= 10_000 {
        3
    } else {
        20
    };
    let expected = crate::oracle::sort_rows(crate::arms::read_rows(&db, query)?);
    let mut recompute_total = 0.0;
    for _ in 0..reps {
        let started = Instant::now();
        let rows = crate::oracle::sort_rows(crate::arms::read_rows(&db, "SELECT * FROM v")?);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if rows != expected {
            bail!("view drifted from recompute at n={n}");
        }
        recompute_total += elapsed;
    }
    let recompute_ms = recompute_total / reps as f64;

    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let db_bytes = std::fs::metadata(&db_path)?.len();
    let arrangement_rows: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name LIKE '__ivm\\_v\\_%' ESCAPE '\\'",
        [],
        |row| row.get(0),
    )?;
    drop(db);
    let rss_delta_bytes = super::arms::peak_rss_bytes()
        .zip(rss_before)
        .map(|(after, before)| after.saturating_sub(before));
    crate::arms::remove_db_files(&db_path);
    Ok(ScaleRow {
        circuit: circuit.to_string(),
        fanout,
        n,
        insert_ms,
        delete_ms,
        update_ms,
        replace_ms,
        recompute_ms,
        rss_delta_bytes,
        db_bytes,
        arrangement_rows,
    })
}
fn render_svgs(out: &Path, rows: &[ScaleRow]) -> Result<bool> {
    let available = std::process::Command::new("gnuplot")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !available {
        info!("gnuplot not found; skipping SVG render");
        return Ok(false);
    }
    let metrics = ["insert", "delete", "update", "replace", "recompute"];
    let mut circuits: Vec<String> = Vec::new();
    for row in rows {
        if !circuits.contains(&row.circuit) {
            circuits.push(row.circuit.clone());
        }
    }
    for circuit in &circuits {
        let cell_rows: Vec<&ScaleRow> = rows.iter().filter(|r| &r.circuit == circuit).collect();
        let mut ns: Vec<i64> = cell_rows.iter().map(|r| r.n).collect();
        ns.sort();
        ns.dedup();
        let mut script = String::from("set terminal svg size 900,600\nset key outside\n");
        script.push_str("set xlabel 'rows n'\nset ylabel 'ms'\n");
        if let (Some(&min), Some(&max)) = (ns.first(), ns.last()) {
            if max / min.max(1) >= 10 {
                script.push_str("set logscale x\n");
            }
        }
        script.push_str(&format!(
            "set output '{}'\n",
            out.join(format!("scale-{circuit}.svg")).display()
        ));
        let mut plot = String::from("plot ");
        let mut first = true;
        for label in metrics {
            for fanout in [1i64, 10] {
                let points: Vec<(i64, f64)> = cell_rows
                    .iter()
                    .filter(|r| r.fanout == fanout)
                    .map(|r| (r.n, metric_ms(r, label)))
                    .collect();
                if points.is_empty() {
                    continue;
                }
                let data: String =
                    points.iter().map(|(n, ms)| format!("{n} {ms:.3}\n")).collect();
                script.push_str(&format!(
                    "$data_{label}_{fanout} << EOD\n{data}EOD\n"
                ));
                if !first {
                    plot.push_str(", ");
                }
                first = false;
                plot.push_str(&format!(
                    "$data_{label}_{fanout} with linespoints title '{label} f={fanout}'"
                ));
            }
        }
        script.push_str(&plot);
        script.push('\n');
        let mut child = std::process::Command::new("gnuplot")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            bail!("gnuplot failed for {circuit}");
        }
    }
    Ok(true)
}


fn metric_ms(row: &ScaleRow, label: &str) -> f64 {
    match label {
        "insert" => row.insert_ms,
        "delete" => row.delete_ms,
        "update" => row.update_ms,
        "replace" => row.replace_ms,
        _ => row.recompute_ms,
    }
}
