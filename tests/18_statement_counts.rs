// One number on every statement. Five fixed scenarios, each run under the
// statements spans (src/0g_statements.rs) and hafley-observe's SQLite trace, then
// reduced per phase, per verb, per site, per object. Pinned counts make a
// change that doubles the statement count fail here instead of a benchmark.
//
// Shape follows tests/13_statements_per_drain.rs: a CountRecorder under a
// capture layer, one fixed workload, counts read back from the events.
//
// The tsv on stdout is the receipt; run with --nocapture. With
// EVERY_STATEMENT_WALL set it times the scenarios with logging off instead,
// and the recipe runs that in both the statements and the spans-compiled-out build.
#![cfg(not(feature = "extension"))]

use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
#[cfg(feature = "statements")]
use hafley_observe::sqlite::SQLITE_TARGET;
#[cfg(feature = "statements")]
use std::collections::BTreeMap;
#[cfg(feature = "statements")]
use hafley_observe::{CountRecorder, FieldStats};
#[cfg(feature = "statements")]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// The input row count the inserts scenarios and the retraction scenario use.
const INPUT_ROWS: i64 = 100;
/// The reach circuit's fanout and n. Fanout 1 is the smallest shape.
const REACH_FANOUT: i64 = 1;
const REACH_ROWS: i64 = 8;

/// The two circuits, verbatim from bench/src/scale.rs's SCALE_QUERIES.
const GROUP_QUERY: &str = "SELECT k AS c0,count(*) AS c1,sum(v) AS c2 FROM a GROUP BY k";
const REACH_QUERY: &str = "WITH RECURSIVE reachable(node) AS (SELECT k FROM b UNION SELECT a.v FROM a JOIN reachable r ON a.k=r.node) SELECT node AS c0 FROM reachable";

const SOURCE_DDL: &str = "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
     CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);\
     CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER)";

/// bench/src/scale.rs CellSpec::seed_row.
fn seed_row(id: i64, groups: i64) -> (i64, i64, i64) {
    (id, (id * 7) % groups, (id * 13) % groups)
}

fn groups(n: i64, fanout: i64) -> i64 {
    n / fanout + 1
}

fn view(db: &Connection, name: &str, sql: &str) -> Result<()> {
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {name} USING sqlite_ivm('{}')",
        sql.replace('\'', "''")
    ))
}

fn seed(db: &Connection, n: i64, fanout: i64) -> Result<()> {
    db.execute_batch("BEGIN")?;
    let g = groups(n, fanout);
    let mut a = db.prepare("INSERT INTO a(id,k,v) VALUES(?1,?2,?3)")?;
    let mut b = db.prepare("INSERT INTO b(id,k,v) VALUES(?1,?2,?3)")?;
    for id in 0..n {
        let (id, k, v) = seed_row(id, g);
        a.execute((id, k, v))?;
        b.execute((id, v, k))?;
    }
    drop(a);
    drop(b);
    db.execute_batch("COMMIT")
}

/// One scenario: a fixed sequence over a fresh connection.
type Scenario = (&'static str, i64, fn(&Connection) -> Result<()>);

/// Scenario 1: the declare path alone, no rows.
fn create_group(db: &Connection) -> Result<()> {
    view(db, "g", GROUP_QUERY)
}

/// Scenario 2: one hundred inserts in one transaction.
fn insert_one_txn(db: &Connection) -> Result<()> {
    view(db, "g", GROUP_QUERY)?;
    db.execute_batch("BEGIN")?;
    let mut a = db.prepare("INSERT INTO a(id,k,v) VALUES(?1,?2,?3)")?;
    for id in 0..INPUT_ROWS {
        let (id, k, v) = seed_row(id, groups(INPUT_ROWS, 1));
        a.execute((id, k, v))?;
    }
    drop(a);
    db.execute_batch("COMMIT")
}

/// Scenario 3: one hundred inserts in one hundred transactions.
fn insert_many_txn(db: &Connection) -> Result<()> {
    view(db, "g", GROUP_QUERY)?;
    let mut a = db.prepare("INSERT INTO a(id,k,v) VALUES(?1,?2,?3)")?;
    for id in 0..INPUT_ROWS {
        let (id, k, v) = seed_row(id, groups(INPUT_ROWS, 1));
        a.execute((id, k, v))?;
    }
    Ok(())
}

/// Scenario 4: one delete that retracts a recursive closure.
fn retract_recursive(db: &Connection) -> Result<()> {
    view(db, "r", REACH_QUERY)?;
    seed(db, INPUT_ROWS, 1)?;
    db.execute_batch("DELETE FROM a WHERE id=0")
}

/// Scenario 5: the reach circuit at fanout 1, the smallest n.
fn reach_small(db: &Connection) -> Result<()> {
    view(db, "r", REACH_QUERY)?;
    seed(db, REACH_ROWS, REACH_FANOUT)
}

fn scenarios() -> [Scenario; 5] {
    [
        ("create_group", 1, create_group),
        ("inserts_one_txn", INPUT_ROWS, insert_one_txn),
        ("inserts_many_txn", INPUT_ROWS, insert_many_txn),
        ("retract_recursive", 1, retract_recursive),
        ("reach_small", REACH_ROWS, reach_small),
    ]
}

fn run(db: &Connection, scenario: fn(&Connection) -> Result<()>) -> Result<()> {
    db.execute_batch(SOURCE_DDL)?;
    scenario(db)
}

#[cfg(feature = "statements")]
fn counts(recorder: &CountRecorder) -> BTreeMap<(String, String), usize> {
    recorder.event_stats(SQLITE_TARGET, tracing::Level::DEBUG, "stmt", ["phase", "verb"])
        .into_iter()
        .map(|([phase, verb], stats)| ((phase, verb), stats.events))
        .collect()
}

/// Raw cumulative SQLite counters, preserved here for comparison with the
/// existing pins. These sums depend on prepared-handle reuse and are not
/// per-execution work measurements.
#[cfg(feature = "statements")]
fn scan_metrics(recorder: &CountRecorder) -> BTreeMap<String, (i64, i64)> {
    recorder.event_stats(SQLITE_TARGET, tracing::Level::DEBUG, "stmt", ["phase"])
        .into_iter()
        .map(|([phase], stats)| (phase, (
            stats.fields.get("vm_step").map_or(0.0, FieldStats::sum) as i64,
            stats.fields.get("fullscan_step").map_or(0.0, FieldStats::sum) as i64,
        )))
        .collect()
}

#[cfg(feature = "statements")]
#[derive(Default)]
struct Cell {
    calls: usize,
    nanos: FieldStats,
    rows: i64,
    rows_known: bool,
    cached: usize,
}

#[cfg(feature = "statements")]
fn cells(recorder: &CountRecorder) -> BTreeMap<(String, String, String, String), Cell> {
    let mut cells = BTreeMap::<(String, String, String, String), Cell>::new();
    for ([phase, verb, site, object, prepared], mut stats) in recorder.event_stats(
        SQLITE_TARGET, tracing::Level::DEBUG, "stmt", ["phase", "verb", "site", "object", "prepared"],
    ) {
        let cell = cells.entry((phase, verb, site, object)).or_default();
        cell.calls += stats.events;
        if let Some(nanos) = stats.fields.remove("nanos") {
            cell.nanos.samples.extend(nanos.samples);
        }
        if let Some(rows) = stats.ancestor_fields.get("rows") {
            cell.rows_known = true;
            cell.rows += rows.sum() as i64;
        }
        if prepared == "cached" {
            cell.cached += stats.events;
        }
    }
    cells
}

/// The tsv the recipe commits: one row per (phase, verb, site, object), raw
/// microseconds, sorted by the indictment column (calls per input row).
#[cfg(feature = "statements")]
fn print_table(
    scenario: &str,
    input_rows: i64,
    cells: &BTreeMap<(String, String, String, String), Cell>,
) {
    let mut rows: Vec<_> = cells
        .iter()
        .map(|(key, cell)| {
            let total_ns = cell.nanos.sum();
            let mean_us = if cell.calls == 0 {
                0.0
            } else {
                total_ns as f64 / cell.calls as f64 / 1000.0
            };
            let prepared_pct = if cell.calls == 0 {
                0.0
            } else {
                100.0 * cell.cached as f64 / cell.calls as f64
            };
            let per_input_row = cell.calls as f64 / input_rows as f64;
            (key, cell, total_ns as f64 / 1000.0, mean_us, prepared_pct, per_input_row)
        })
        .collect();
    rows.sort_by(|a, b| b.5.partial_cmp(&a.5).unwrap_or(std::cmp::Ordering::Equal));
    for (key, cell, total_us, mean_us, prepared_pct, per_input_row) in rows {
        let (phase, verb_name, site, object) = key;
        let rows = if cell.rows_known {
            cell.rows.to_string()
        } else {
            String::new()
        };
        println!(
            "{scenario}\t{phase}\t{verb_name}\t{site}\t{object}\t{}\t{total_us:.1}\t{mean_us:.1}\t{:.1}\t{rows}\t{prepared_pct:.1}\t{per_input_row:.2}",
            cell.calls,
            cell.nanos.percentile(99.0).unwrap_or_default() / 1000.0,
        );
    }
}

#[cfg(feature = "statements")]
fn recorded_counts(recorder: &CountRecorder) -> serde_json::Value {
    serde_json::json!({
        "counts": counts(recorder).into_iter().map(|((phase, verb), calls)| (phase, verb, calls)).collect::<Vec<_>>(),
        "scans": scan_metrics(recorder).into_iter().map(|(phase, (vm, scan))| (phase, vm, scan)).collect::<Vec<_>>(),
    })
}

#[cfg(feature = "statements")]
fn record(scenario: fn(&Connection) -> Result<()>) -> Result<CountRecorder> {
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    let db = Connection::open_in_memory()?;
    register(&db)?;
    hafley_observe::sqlite::instrument(&db);
    run(&db, scenario)?;
    Ok(recorder)
}

/// The wall-time pass: logging off, three runs per scenario, the median and
/// the spread printed. The recipe runs it in the statements build and in the
/// spans-compiled-out build, so the page names which wall came from which.
fn wall() -> Result<()> {
    const RUNS: usize = 3;
    for (name, _input_rows, scenario) in scenarios() {
        let mut samples = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let db = Connection::open_in_memory()?;
            register(&db)?;
            let clock = std::time::Instant::now();
            run(&db, scenario)?;
            samples.push(clock.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "WALL\t{name}\t{:.3}\t{:.3}\t{:.3}",
            samples[0],
            samples[RUNS / 2],
            samples[RUNS - 1]
        );
    }
    Ok(())
}

#[test]
fn statement_counts_match_pinned() -> Result<()> {
    if std::env::var_os("EVERY_STATEMENT_WALL").is_some() {
        return wall();
    }
    #[cfg(feature = "statements")]
    {
        counts_pass()
    }
    #[cfg(not(feature = "statements"))]
    {
        Ok(())
    }
}

#[cfg(feature = "statements")]
fn counts_pass() -> Result<()> {
    let pinned: serde_json::Value = serde_json::from_str(include_str!("fixtures/3_statement_counts.json")).unwrap();
    for (name, input_rows, scenario) in scenarios() {
        let recorder = record(scenario)?;
        print_table(name, input_rows, &cells(&recorder));
        let actual = recorded_counts(&recorder);
        assert!(!actual["counts"].as_array().unwrap().is_empty(), "{name} issued no instrumented statement");
        for (phase, (vm_step, fullscan_step)) in scan_metrics(&recorder) {
            println!("SCAN\t{name}\t{phase}\t{vm_step}\t{fullscan_step}");
        }
        assert_eq!(actual, pinned[name], "statement counts for {name}");
    }
    Ok(())
}

/// Refresh only after reviewing the SQL execution changes, then review the diff.
#[cfg(feature = "statements")]
#[test]
#[ignore]
fn refresh_statement_counts() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = serde_json::Map::new();
    for (name, _, scenario) in scenarios() {
        fixture.insert(name.into(), recorded_counts(&record(scenario)?));
    }
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/3_statement_counts.json"),
        serde_json::to_string_pretty(&fixture)? + "\n",
    )?;
    Ok(())
}
