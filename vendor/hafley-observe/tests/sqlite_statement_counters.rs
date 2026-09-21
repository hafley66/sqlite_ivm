#![cfg(feature = "sqlite-sink")]

use hafley_observe::sqlite::{query_plan, StatementFinding};
use rusqlite::Connection;
use tracing_capture::{CaptureLayer, SharedStorage};
use tracing_subscriber::prelude::*;

fn seeded_connection() -> Connection {
    let connection = Connection::open_in_memory().expect("in-memory database");
    connection
        .execute_batch(
            "CREATE TABLE arrangement(group_key TEXT, value INTEGER, multiplicity INTEGER);
             CREATE INDEX arrangement_group ON arrangement(group_key, value);
             CREATE TABLE unindexed(value INTEGER);",
        )
        .expect("schema");
    let mut insert = connection
        .prepare("INSERT INTO arrangement VALUES('g', ?1, 1)")
        .expect("prepare");
    for value in 0..200 {
        insert.execute([value]).expect("seed arrangement");
    }
    drop(insert);
    let mut insert = connection
        .prepare("INSERT INTO unindexed VALUES(?1)")
        .expect("prepare");
    for value in 0..200 {
        insert.execute([value]).expect("seed unindexed");
    }
    drop(insert);
    connection
}

fn warnings_for(sql: &str) -> Vec<String> {
    let storage = SharedStorage::default();
    let subscriber = tracing_subscriber::registry().with(CaptureLayer::new(&storage));
    tracing::subscriber::with_default(subscriber, || {
        let connection = seeded_connection();
        hafley_observe::sqlite::instrument(&connection);
        connection
            .prepare(sql)
            .expect("prepare")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("query")
            .for_each(|row| {
                row.expect("row");
            });
        hafley_observe::sqlite::silence(&connection);
    });
    let storage = storage.lock();
    storage
        .all_events()
        .filter(|event| *event.metadata().level() == tracing::Level::WARN)
        .filter_map(|event| event.message().map(|message| message.to_string()))
        .collect()
}

#[test]
fn an_unindexed_scan_reports_a_table_scan() {
    let warnings = warnings_for("SELECT value FROM unindexed WHERE value > 100");
    assert!(
        warnings.iter().any(|w| w == StatementFinding::TableScan.as_str()),
        "expected a table-scan finding, got {warnings:?}"
    );
}

#[test]
fn an_unindexed_order_by_reports_a_temporary_btree_sort() {
    let warnings = warnings_for("SELECT value FROM unindexed ORDER BY value DESC");
    assert!(
        warnings
            .iter()
            .any(|w| w == StatementFinding::TemporaryBtreeSort.as_str()),
        "expected a sort finding, got {warnings:?}"
    );
}

#[test]
fn an_indexed_lookup_reports_nothing() {
    let warnings = warnings_for("SELECT value FROM arrangement WHERE group_key = 'g'");
    assert!(warnings.is_empty(), "expected no findings, got {warnings:?}");
}

#[test]
fn the_planner_account_names_the_temporary_btree() {
    let connection = seeded_connection();
    let plan = query_plan(&connection, "SELECT value FROM unindexed ORDER BY value DESC")
        .expect("query plan");
    assert!(
        plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "expected a temp b-tree step, got {plan:?}"
    );
}
