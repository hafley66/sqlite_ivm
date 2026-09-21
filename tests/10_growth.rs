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

/// One drain span per autocommit statement and no more. A path that re-walked
/// what it had already seen would read Quadratic here.
#[test]
fn drain_spans_stay_linear_in_changed_rows() -> Result<()> {
    let small = counts_for(SMALL_ROWS)?;
    let large = counts_for(SMALL_ROWS * SIZE_RATIO as i64)?;
    assert_growth(&small, &large, "drain", SIZE_RATIO, Growth::Linear);
    Ok(())
}
