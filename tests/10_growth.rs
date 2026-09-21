// Counts, never durations: a span count is deterministic where elapsed time is not.
// Each case owns its subscriber guard, so the two folds cannot see each other's spans.
#![cfg(not(feature = "extension"))]
use hafley_observe::sqlite::{instrument, SQLITE_TARGET};
use hafley_observe::{assert_growth, CountRecorder, EventSums, Growth, SpanCounts};
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use std::collections::BTreeMap;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Rows the small fold inserts, and the factor the large one multiplies it by.
const SMALL_ROWS: i64 = 8;
const SIZE_RATIO: f64 = 16.0;
const GROUP_MULTIPLICITY_MAX: i64 = SMALL_ROWS * SIZE_RATIO as i64;

fn fold(rows: i64) -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);\
         CREATE VIRTUAL TABLE top USING sqlite_ivm(\
           'SELECT v AS value FROM a ORDER BY value LIMIT 3');",
    )?;
    let mut insert = db.prepare("INSERT INTO a(id,v) VALUES(?1,?2)")?;
    // Two distinct values, so every insert past the second raises a multiplicity
    // rather than adding a group.
    for id in 0..rows {
        insert.execute((id, id % 2))?;
    }
    Ok(())
}

fn counts_for(rows: i64) -> Result<SpanCounts> {
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    fold(rows)?;
    Ok(recorder.counts())
}

fn group_limit_counts_for(rows: i64) -> Result<SpanCounts> {
    assert!(
        rows <= GROUP_MULTIPLICITY_MAX,
        "GROUP_MULTIPLICITY_MAX bounds the recursive input generator"
    );
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);\
         CREATE VIRTUAL TABLE top USING sqlite_ivm(\
           'SELECT v AS value FROM a ORDER BY value LIMIT 3');",
    )?;
    db.execute(
        "WITH RECURSIVE seq(id) AS (\
           SELECT 0 UNION ALL SELECT id+1 FROM seq WHERE id+1<?1\
         ) INSERT INTO a SELECT id,0 FROM seq",
        [rows],
    )?;
    let group_entries = recorder
        .span_counts_by_field("node", "kind")
        .get("group_limit")
        .copied()
        .unwrap_or_default();
    let mut counts = SpanCounts::default();
    counts
        .entries
        .insert("group_limit".to_string(), group_entries);
    Ok(counts)
}

/// One drain span per autocommit statement and no more. A path that re-walked
/// what it had already seen would read Quadratic here.
#[test]
fn drain_spans_stay_linear_in_changed_rows() -> Result<()> {
    let small = counts_for(SMALL_ROWS)?;
    let large = counts_for(SMALL_ROWS * SIZE_RATIO as i64)?;
    assert_growth(&small, &large, "drain", SIZE_RATIO, Growth::Linear);
    Ok(())
}

#[test]
fn group_limit_entries_stay_constant_in_input_multiplicity() -> Result<()> {
    let small = group_limit_counts_for(SMALL_ROWS)?;
    let large = group_limit_counts_for(GROUP_MULTIPLICITY_MAX)?;
    assert_growth(
        &small,
        &large,
        "group_limit",
        SIZE_RATIO,
        Growth::Constant,
    );
    assert_eq!(
        small.entries_of("group_limit"),
        1,
        "one Group LIMIT entry per drain"
    );
    assert_eq!(
        large.entries_of("group_limit"),
        1,
        "one Group LIMIT entry per drain"
    );
    Ok(())
}

/// One view per node kind, so one drain exercises every phase.
const VIEWS: [(&str, &str); 8] = [
    ("window_view", "SELECT id, ROW_NUMBER() OVER (PARTITION BY v ORDER BY id) AS rn FROM a"),
    ("reach_view", "WITH RECURSIVE p(x,y) AS (SELECT id,v FROM a UNION SELECT p.x,a.v FROM p JOIN a ON a.id=p.y) SELECT x,y FROM p"),
    ("map_view", "SELECT v+1 AS w FROM a"),
    ("group_view", "SELECT v, count(*) AS n FROM a GROUP BY v"),
    ("join_view", "SELECT a.id, b.k FROM a JOIN b ON b.k=a.v"),
    ("distinct_view", "SELECT DISTINCT v FROM a"),
    ("limit_view", "SELECT v AS value FROM a ORDER BY value LIMIT 3"),
    ("union_view", "SELECT v FROM a UNION ALL SELECT k FROM b"),
];

/// Span instances of the `node` span per kind, at either batch size, for the
/// eight views above. One span per node, not one per row: a kind that starts
/// building a span inside the row loop moves this map.
const NODE_INSTANCES: [(&str, usize); 11] = [
    ("apply_state", 8),
    ("fixpoint", 1),
    ("group", 1),
    ("group_limit", 1),
    ("join", 1),
    ("map", 20),
    ("seed", 8),
    ("set", 1),
    ("sweep", 8),
    ("union_all", 1),
    ("window", 1),
];

/// Every statement the drain ran, keyed by (drain view, sql). The empty view
/// key holds registration DDL, which no drain encloses.
type DrainStatements = BTreeMap<(String, String), EventSums>;

/// Builds the eight views, runs one transaction of `rows` inserts into `a`,
/// and returns the span counts, the per-statement counters, and the node span
/// instances per kind. Nothing here reads a clock.
fn drain_statements(
    rows: i64,
) -> Result<(SpanCounts, DrainStatements, BTreeMap<String, usize>)> {
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    let db = Connection::open_in_memory()?;
    register(&db)?;
    instrument(&db);
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);\
         CREATE TABLE b(k INTEGER);INSERT INTO b VALUES(0),(1)",
    )?;
    for (name, sql) in VIEWS {
        db.execute_batch(&format!("CREATE VIRTUAL TABLE {name} USING sqlite_ivm('{sql}')"))?;
    }
    db.execute_batch("BEGIN")?;
    let mut insert = db.prepare("INSERT INTO a(id,v) VALUES(?1,?2)")?;
    for id in 0..rows {
        insert.execute((id, id))?;
    }
    drop(insert);
    db.execute_batch("COMMIT")?;
    let statements = recorder.event_sums(
        SQLITE_TARGET,
        tracing::Level::DEBUG,
        "drain",
        "view",
        Some("sql"),
    );
    Ok((
        recorder.counts(),
        statements,
        recorder.span_counts_by_field("node", "kind"),
    ))
}

/// The counters for one statement, as one sortable string.
fn triple(events: usize, sums: &EventSums) -> String {
    format!(
        "{}/{}/{}",
        sums.sum_of("vm_step") as i64,
        sums.sum_of("fullscan_step") as i64,
        events
    )
}

fn statement_triples(statements: &DrainStatements) -> BTreeMap<String, Vec<String>> {
    let mut by_view: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((view, _), sums) in statements {
        if view.is_empty() {
            continue;
        }
        by_view
            .entry(view.clone())
            .or_default()
            .push(triple(sums.events, sums));
    }
    for triples in by_view.values_mut() {
        triples.sort_by_key(|entry| {
            entry
                .split('/')
                .map(|part| part.parse::<i64>().unwrap_or_default())
                .collect::<Vec<_>>()
        });
    }
    by_view
}

/// Level 3 of the timeout rail, the deterministic half. `vm_step` is SQLite's
/// own opcode count for a statement: the same input yields the same count on
/// every machine and on every run, unlike elapsed time. So this pin is exact.
/// One run, no tolerance band, no repeat, nothing that machine load can move.
#[test]
fn drain_statement_costs_match_the_pinned_counts() -> Result<()> {
    let (_, statements, _) = drain_statements(SMALL_ROWS)?;
    let measured = statement_triples(&statements);
    let pinned: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/2_statement_costs.json")).unwrap();
    let mut failures = vec![];
    for (view, expected) in pinned["views"].as_object().unwrap() {
        let expected: Vec<String> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_string())
            .collect();
        let actual = measured.get(view).cloned().unwrap_or_default();
        if actual == expected {
            continue;
        }
        let (actual_counts, expected_counts) = (counts_of(&actual), counts_of(&expected));
        let mut heaviest: Vec<(i64, String)> = statements
            .iter()
            .filter(|((name, _), _)| name == view)
            .map(|((_, sql), sums)| {
                (
                    sums.sum_of("vm_step") as i64,
                    format!("{} {}", triple(sums.events, sums), one_line(sql)),
                )
            })
            .collect();
        heaviest.sort_by(|left, right| right.0.cmp(&left.0));
        let heaviest = heaviest
            .into_iter()
            .take(8)
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        failures.push(format!(
            "{view}: {} statements measured, {} pinned\n  measured only: {:?}\n  pinned only:   {:?}\n  heaviest measured: {}",
            actual.len(),
            expected.len(),
            differing(&actual_counts, &expected_counts),
            differing(&expected_counts, &actual_counts),
            heaviest.join(" | ")
        ));
    }
    assert!(
        failures.is_empty(),
        "statement costs moved:\n{}",
        failures.join("\n")
    );
    Ok(())
}

fn counts_of(list: &[String]) -> BTreeMap<&str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in list {
        *counts.entry(entry.as_str()).or_default() += 1;
    }
    counts
}

fn differing(left: &BTreeMap<&str, usize>, right: &BTreeMap<&str, usize>) -> Vec<String> {
    let mut out = vec![];
    for (entry, count) in left {
        if right.get(entry).copied().unwrap_or_default() != *count {
            out.push(format!("{entry} x{count}"));
        }
    }
    out
}

fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Level 3, deterministic half: the class each phase grows at between two
/// batch sizes, and the class each view's opcode count grows at. One run per
/// size. A phase that turns quadratic changes its class here without a clock
/// being read.
#[test]
fn phase_and_cost_growth_classes_hold() -> Result<()> {
    let (small_spans, small_statements, small_kinds) = drain_statements(SMALL_ROWS)?;
    let (large_spans, large_statements, large_kinds) =
        drain_statements(SMALL_ROWS * SIZE_RATIO as i64)?;
    for (phase, expected) in [
        ("drain", Growth::Constant),
        ("node", Growth::Constant),
        ("fixpoint", Growth::Constant),
        ("round", Growth::Constant),
    ] {
        assert_growth(&small_spans, &large_spans, phase, SIZE_RATIO, expected);
    }
    for (kind, expected) in NODE_INSTANCES {
        assert_eq!(
            small_kinds.get(kind).copied().unwrap_or_default(),
            expected,
            "node span instances of kind {kind} at {SMALL_ROWS} rows"
        );
        assert_eq!(
            large_kinds.get(kind).copied().unwrap_or_default(),
            expected,
            "node span instances of kind {kind} at {} rows",
            SMALL_ROWS * SIZE_RATIO as i64
        );
    }
    let cost = |statements: &DrainStatements| -> SpanCounts {
        let mut counts = SpanCounts::default();
        for ((view, _), sums) in statements {
            if !view.is_empty() {
                *counts.entries.entry(view.clone()).or_default() +=
                    sums.sum_of("vm_step") as usize;
            }
        }
        counts
    };
    let (small_cost, large_cost) = (cost(&small_statements), cost(&large_statements));
    assert_eq!(small_cost.entries.len(), 8, "one cost pin per view");
    for view in small_cost.entries.keys() {
        assert_growth(&small_cost, &large_cost, view, SIZE_RATIO, Growth::Linear);
    }
    Ok(())
}

/// Rewrites the pinned statement costs. Run it by hand only after a change to
/// a statement's work is understood, then commit the file:
/// `cargo nextest run --run-ignored ignored-only -E 'test(=refresh_statement_costs)'`
#[test]
#[ignore]
fn refresh_statement_costs() -> Result<(), Box<dyn std::error::Error>> {
    let (_, statements, _) = drain_statements(SMALL_ROWS)?;
    let views = statement_triples(&statements);
    let fixture = serde_json::json!({
        "scenario": "tests/10_growth.rs drain_statement_costs_match_the_pinned_counts",
        "format": "one entry per distinct statement: vm_step/fullscan_step/events, sorted numerically",
        "views": views,
    });
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/2_statement_costs.json");
    std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n")?;
    Ok(())
}
