// Set-at-a-time law: past the seed node, a drain runs the same statements
// however many source rows the batch carries. Seed lands one row per statement.
#![cfg(not(feature = "extension"))]
use hafley_observe::sqlite::SQLITE_TARGET;
use hafley_observe::CountRecorder;
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const SMALL_ROWS: i64 = 8;
const SIZE_RATIO: i64 = 16;

/// One view per node kind the drain distinguishes.
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

fn transaction(db: &Connection, rows: i64) -> Result<()> {
    db.execute_batch("BEGIN")?;
    let mut insert = db.prepare("INSERT INTO a(id,v) VALUES(?1,?2)")?;
    // Every row its own group, so a per-group statement grows with the batch too.
    for id in 0..rows {
        insert.execute((id, id))?;
    }
    drop(insert);
    db.execute_batch("COMMIT")
}

/// Statements finished inside each node span other than seed, keyed by view.
fn statements_per_drain(rows: i64) -> Result<Vec<(String, usize)>> {
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();
    let db = Connection::open_in_memory()?;
    register(&db)?;
    hafley_observe::sqlite::instrument(&db);
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);\
         CREATE TABLE b(k INTEGER);INSERT INTO b VALUES(0),(1)",
    )?;
    for (name, sql) in VIEWS {
        db.execute_batch(&format!("CREATE VIRTUAL TABLE {name} USING sqlite_ivm('{sql}')"))?;
    }
    transaction(&db, rows)?;
    let by_statement =
        recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "drain", "view", Some("sql"));
    let mut per_view = std::collections::BTreeMap::<String, usize>::new();
    for ((view, sql), sums) in by_statement {
        // The seed insert is the one statement bound per row.
        let seed = sql.starts_with("INSERT INTO temp.__ivm_out_") && sql.contains(" VALUES(?");
        if !view.is_empty() && !seed {
            *per_view.entry(view).or_default() += sums.events;
        }
    }
    Ok(per_view.into_iter().collect())
}

fn timing_workload(rows: i64) -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);\
         CREATE TABLE b(k INTEGER);INSERT INTO b VALUES(0),(1)",
    )?;
    for (name, sql) in VIEWS {
        db.execute_batch(&format!("CREATE VIRTUAL TABLE {name} USING sqlite_ivm('{sql}')"))?;
    }
    transaction(&db, rows)
}

#[test]
fn statements_per_drain_do_not_grow_with_the_batch() -> Result<()> {
    let small = statements_per_drain(SMALL_ROWS)?;
    let large = statements_per_drain(SMALL_ROWS * SIZE_RATIO)?;
    assert_eq!(small.len(), VIEWS.len(), "every view drains once: {small:?}");
    let mut grew = vec![];
    for ((view, before), (_, after)) in small.iter().zip(&large) {
        if after != before {
            grew.push(format!("{view}: {before} statements at {SMALL_ROWS} rows, {after} at {}", SMALL_ROWS * SIZE_RATIO));
        }
    }
    assert!(grew.is_empty(), "statements grew with the batch:\n{}", grew.join("\n"));
    if std::env::var_os("HAFLEY_LOG").is_some() {
        timing_workload(SMALL_ROWS * SIZE_RATIO)?;
    }
    Ok(())
}
