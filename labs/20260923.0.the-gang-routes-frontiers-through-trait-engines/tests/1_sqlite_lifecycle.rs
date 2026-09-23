use lab_20260923_0::{Change, FrontierEngine, Plan, SourceRow, SqliteEngine};

fn direct(id: i64, person: i64, resource: i64) -> Change {
    Change {
        source: 2,
        row: SourceRow {
            id,
            cells: vec![person, resource],
        },
        weight: 1,
    }
}

fn installed() -> SqliteEngine {
    let mut engine = SqliteEngine::memory().unwrap();
    engine
        .install(Plan::JoinUnion {
            left: 0,
            right: 1,
            direct: 2,
            left_key: 1,
            right_key: 0,
            left_output: 0,
            right_output: 1,
            direct_output: [0, 1],
        })
        .unwrap();
    engine
}

#[test]
fn collector_respects_savepoint_and_transaction_rollback() {
    let mut engine = installed();
    engine.apply(&[direct(1, 1, 100)]).unwrap();
    let baseline = engine.snapshot().unwrap();
    let before = sqlite_ext::counts(engine.connection(), "iso_watch").unwrap();
    engine
        .connection()
        .execute_batch(
            "BEGIN;
         SAVEPOINT discarded;
         INSERT INTO src_2 VALUES(2,2,200);
         ROLLBACK TO discarded;
         RELEASE discarded;
         COMMIT;",
        )
        .unwrap();
    assert_eq!(engine.snapshot().unwrap(), baseline);
    engine
        .connection()
        .execute_batch("BEGIN; INSERT INTO src_2 VALUES(3,3,300); ROLLBACK;")
        .unwrap();
    assert_eq!(engine.snapshot().unwrap(), baseline);
    let after = sqlite_ext::counts(engine.connection(), "iso_watch").unwrap();
    assert!(after.rollback_to > before.rollback_to);
    assert!(after.rollback > before.rollback);
    engine.apply(&[direct(2, 2, 200)]).unwrap();
    assert_eq!(engine.snapshot().unwrap(), vec![vec![1, 100], vec![2, 200]]);
}

#[test]
fn failed_sync_rolls_back_source_and_result_then_recovers() {
    let mut engine = installed();
    engine.apply(&[direct(1, 1, 100)]).unwrap();
    let baseline = engine.snapshot().unwrap();
    engine
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_iso_outbox BEFORE INSERT ON __iso_outbox
         BEGIN SELECT RAISE(ABORT,'injected maintenance failure'); END;",
        )
        .unwrap();
    let error = engine.apply(&[direct(2, 2, 200)]).unwrap_err();
    assert!(
        error.message.contains("injected maintenance failure"),
        "{error}"
    );
    assert_eq!(engine.snapshot().unwrap(), baseline);
    let source_count: i64 = engine
        .connection()
        .query_row("SELECT count(*) FROM src_2", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_count, 1);
    engine
        .connection()
        .execute_batch("DROP TRIGGER fail_iso_outbox")
        .unwrap();
    engine.apply(&[direct(2, 2, 200)]).unwrap();
    assert_eq!(engine.snapshot().unwrap(), vec![vec![1, 100], vec![2, 200]]);
    let outbox_rows: i64 = engine
        .connection()
        .query_row("SELECT count(*) FROM __iso_outbox", [], |row| row.get(0))
        .unwrap();
    assert_eq!(outbox_rows, 0);
}

#[test]
fn installed_graph_has_bounded_object_count() {
    let engine = installed();
    let db = engine.connection();
    let count = |kind| {
        db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type=?1",
            [kind],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert!(count("table") <= 12, "table count = {}", count("table"));
    assert!(count("index") <= 8, "index count = {}", count("index"));
    assert_eq!(count("trigger"), 9);
}
