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
use tracing::info;


pub const DEFAULT_ENGINES: [&str; 5] = ["sqlite-ivm", "pg-ivm", "sqlite-query", "pg-query", "dd"];
pub struct Shootout {
    pub smoke: bool,
    pub engines: Vec<String>,
    pub out: PathBuf,
    pub circuits: Option<Vec<String>>,
    pub pg_prefix: Option<PathBuf>,
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
