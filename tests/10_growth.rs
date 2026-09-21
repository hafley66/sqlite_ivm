// Counts, never durations: a span count is deterministic where elapsed time is not.
// Each case owns its subscriber guard, so the two folds cannot see each other's spans.
#![cfg(not(feature = "extension"))]
use hafley_observe::{assert_growth, CountRecorder, Growth, SpanCounts};
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
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
