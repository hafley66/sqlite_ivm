// One number on every statement. Five fixed scenarios, each run under the
// census spans (src/0g_census.rs) and hafley-observe's SQLite trace, then
// reduced per phase, per verb, per site, per object. Pinned counts make a
// change that doubles the statement count fail here instead of a benchmark.
//
// Shape follows tests/13_statements_per_drain.rs: a CountRecorder under a
// capture layer, one fixed workload, counts read back from the events.
//
// The tsv on stdout is the receipt; run with --nocapture.
#![cfg(all(not(feature = "extension"), feature = "census"))]

use hafley_observe::sqlite::SQLITE_TARGET;
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use std::collections::BTreeMap;
use tracing_capture::{CapturedEvent, CapturedSpan, SharedStorage};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// The input row count the inserts scenarios and the retraction scenario use.
const INPUT_ROWS: i64 = 100;
/// The reach circuit's fanout and n. Fanout 1 is the smallest shape.
const REACH_FANOUT: i64 = 1;
const REACH_ROWS: i64 = 8;
/// Statement time on this SQLite build is quantized to one millisecond, so a
/// cell whose total falls below this floor reads as no effect at this scale.
const TIME_FLOOR_US: f64 = 1000.0;

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

/// Per (phase, verb), the number of statement executions.
fn counts(storage: &SharedStorage) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    let storage = storage.lock();
    for event in storage.all_events() {
        if !is_statement_event(event.metadata()) {
            continue;
        }
        let Some(span) = event
            .ancestors()
            .find(|span| span.metadata().name() == "stmt")
        else {
            continue;
        };
        *counts
            .entry((span_field(span, "phase"), span_field(span, "verb")))
            .or_default() += 1;
    }
    counts
}

fn is_statement_event(meta: &tracing::Metadata<'_>) -> bool {
    meta.target() == SQLITE_TARGET && *meta.level() == tracing::Level::DEBUG
}

fn span_field(span: CapturedSpan<'_>, name: &str) -> String {
    let Some(value) = span.value(name) else {
        return String::new();
    };
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_debug_str().map(str::to_string))
        .unwrap_or_else(|| format!("{value:?}"))
}

fn event_int(event: &CapturedEvent<'_>, name: &str) -> i64 {
    let Some(value) = event.value(name) else {
        return 0;
    };
    if let Some(n) = value.as_int() {
        return n as i64;
    }
    if let Some(n) = value.as_uint() {
        return n as i64;
    }
    if let Some(f) = value.as_float() {
        return f as i64;
    }
    0
}

#[derive(Default)]
struct Cell {
    calls: usize,
    nanos: Vec<i64>,
    rows: i64,
    cached: usize,
}

/// Every statement execution, grouped by phase, verb, site and object, read
/// off the capture layer's raw events so the per-call tail survives.
fn cells(storage: &SharedStorage) -> BTreeMap<(String, String, String, String), Cell> {
    let mut cells: BTreeMap<(String, String, String, String), Cell> = BTreeMap::new();
    let storage = storage.lock();
    for event in storage.all_events() {
        if !is_statement_event(event.metadata()) {
            continue;
        }
        let Some(span) = event
            .ancestors()
            .find(|span| span.metadata().name() == "stmt")
        else {
            continue;
        };
        let key = (
            span_field(span, "phase"),
            span_field(span, "verb"),
            span_field(span, "site"),
            span_field(span, "object"),
        );
        let cell = cells.entry(key).or_default();
        cell.calls += 1;
        cell.nanos.push(event_int(&event, "nanos"));
        cell.rows += span
            .value("rows")
            .and_then(|value| value.as_uint().map(|n| n as i64))
            .unwrap_or_default();
        if span_field(span, "prepared") == "cached" {
            cell.cached += 1;
        }
    }
    cells
}

fn p99(nanos: &[i64]) -> f64 {
    if nanos.is_empty() {
        return 0.0;
    }
    let mut sorted = nanos.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * 0.99).ceil() as usize).saturating_sub(1);
    sorted[index] as f64
}

/// The tsv the recipe commits: one row per (phase, verb, site, object).
fn print_table(scenario: &str, input_rows: i64, cells: &BTreeMap<(String, String, String, String), Cell>) {
    let mut rows: Vec<_> = cells
        .iter()
        .map(|(key, cell)| {
            let total_ns: i64 = cell.nanos.iter().sum();
            let mean_us = if cell.calls == 0 {
                0.0
            } else {
                total_ns as f64 / cell.calls as f64 / 1000.0
            };
            let total_us = total_ns as f64 / 1000.0;
            let total_ms = total_ns as f64 / 1_000_000.0;
            let prepared_pct = if cell.calls == 0 {
                0.0
            } else {
                100.0 * cell.cached as f64 / cell.calls as f64
            };
            let per_input_row = cell.calls as f64 / input_rows as f64;
            (
                key,
                cell,
                total_ns,
                total_us,
                total_ms,
                mean_us,
                prepared_pct,
                per_input_row,
            )
        })
        .collect();
    // Sort by the indictment column: calls per input row.
    rows.sort_by(|a, b| b.7.partial_cmp(&a.7).unwrap_or(std::cmp::Ordering::Equal));
    for (key, cell, _total_ns, total_us, total_ms, mean_us, prepared_pct, per_input_row) in rows {
        let (phase, verb_name, site, object) = key;
        let total_ms = if total_us < TIME_FLOOR_US {
            "no effect at this scale".to_string()
        } else {
            format!("{total_ms:.3}")
        };
        println!(
            "{scenario}\t{phase}\t{verb_name}\t{site}\t{object}\t{}\t{total_ms}\t{mean_us:.1}\t{:.1}\t{}\t{prepared_pct:.1}\t{per_input_row:.2}",
            cell.calls,
            p99(&cell.nanos) / 1000.0,
            cell.rows,
        );
    }
}

/// Counts measured on this tree, per (phase, verb). A change that moves any of
/// these numbers fails here; the numbers are the measurement, not an aim.
fn pinned_counts(name: &str) -> Vec<(&'static str, &'static str, usize)> {
    match name {
        "create_group" => vec![
            ("declare", "CREATE", 32),
            ("declare", "DELETE", 4),
            ("declare", "INSERT", 16),
            ("declare", "SELECT", 27),
            ("materialize", "DELETE", 4),
            ("materialize", "INSERT", 6),
            ("materialize", "SELECT", 3),
        ],
        "inserts_one_txn" => vec![
            ("declare", "CREATE", 32),
            ("declare", "DELETE", 4),
            ("declare", "INSERT", 16),
            ("declare", "SELECT", 28),
            ("drain", "DELETE", 4),
            ("drain", "INSERT", 100),
            ("drain", "SELECT", 3),
            ("maintain", "DELETE", 8),
            ("maintain", "INSERT", 8),
            ("maintain", "SELECT", 4),
            ("maintain", "UPDATE", 1),
            ("materialize", "DELETE", 4),
            ("materialize", "INSERT", 10),
            ("materialize", "SELECT", 3),
        ],
        "inserts_many_txn" => vec![
            ("declare", "CREATE", 32),
            ("declare", "DELETE", 4),
            ("declare", "INSERT", 16),
            ("declare", "SELECT", 127),
            ("drain", "DELETE", 400),
            ("drain", "INSERT", 100),
            ("drain", "SELECT", 300),
            ("maintain", "DELETE", 800),
            ("maintain", "INSERT", 800),
            ("maintain", "SELECT", 400),
            ("maintain", "UPDATE", 100),
            ("materialize", "DELETE", 4),
            ("materialize", "INSERT", 406),
            ("materialize", "SELECT", 3),
        ],
        "retract_recursive" => vec![
            ("declare", "CREATE", 64),
            ("declare", "DELETE", 9),
            ("declare", "INSERT", 30),
            ("declare", "SELECT", 51),
            ("drain", "DELETE", 18),
            ("drain", "INSERT", 201),
            ("drain", "SELECT", 16),
            ("fixpoint", "DELETE", 17),
            ("fixpoint", "INSERT", 33),
            ("fixpoint", "SELECT", 19),
            ("fixpoint", "UPDATE", 3),
            ("maintain", "DELETE", 1),
            ("maintain", "SELECT", 3),
            ("materialize", "DELETE", 9),
            ("materialize", "INSERT", 22),
            ("materialize", "SELECT", 6),
        ],
        "reach_small" => vec![
            ("declare", "CREATE", 64),
            ("declare", "DELETE", 9),
            ("declare", "INSERT", 30),
            ("declare", "SELECT", 50),
            ("drain", "DELETE", 9),
            ("drain", "INSERT", 16),
            ("drain", "SELECT", 8),
            ("fixpoint", "DELETE", 8),
            ("fixpoint", "INSERT", 17),
            ("fixpoint", "SELECT", 9),
            ("fixpoint", "UPDATE", 2),
            ("maintain", "DELETE", 1),
            ("maintain", "SELECT", 3),
            ("materialize", "DELETE", 9),
            ("materialize", "INSERT", 21),
            ("materialize", "SELECT", 6),
        ],
        other => panic!("no pinned counts for scenario {other}"),
    }
}

fn pinned(name: &str, counts: &BTreeMap<(String, String), usize>) {
    let expected: BTreeMap<(String, String), usize> = pinned_counts(name)
        .into_iter()
        .map(|(phase, verb_name, calls)| ((phase.to_string(), verb_name.to_string()), calls))
        .collect();
    assert_eq!(*counts, expected, "statement census for {name}");
}

#[test]
fn statement_census_matches_pinned_counts() -> Result<()> {
    for (name, input_rows, scenario) in scenarios() {
        let storage = SharedStorage::default();
        let _guard = tracing_subscriber::registry()
            .with(tracing_capture::CaptureLayer::new(&storage))
            .set_default();
        let db = Connection::open_in_memory()?;
        register(&db)?;
        hafley_observe::sqlite::instrument(&db);
        run(&db, scenario)?;
        let counts = counts(&storage);
        print_table(name, input_rows, &cells(&storage));
        assert!(!counts.is_empty(), "{name} issued no instrumented statement");
        pinned(name, &counts);
    }
    Ok(())
}