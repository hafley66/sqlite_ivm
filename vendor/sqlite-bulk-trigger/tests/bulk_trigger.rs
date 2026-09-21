use rusqlite::{types::Value, Connection};
use sqlite_bulk_trigger::{
    counts, watch, BulkTrigger, Collector, Counts, RowChange, Sign, Watch,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Recorder {
    batches: Arc<Mutex<Vec<Vec<RowChange>>>>,
}

impl Recorder {
    fn batches(&self) -> Vec<Vec<RowChange>> {
        self.batches.lock().unwrap().clone()
    }

    fn only(&self) -> Vec<RowChange> {
        let batches = self.batches();
        assert_eq!(batches.len(), 1);
        batches.into_iter().next().unwrap()
    }
}

impl BulkTrigger for Recorder {
    fn on_batch(&mut self, _db: &Connection, batch: &[RowChange]) -> rusqlite::Result<()> {
        self.batches.lock().unwrap().push(batch.to_vec());
        Ok(())
    }
}

struct Auditor;

impl BulkTrigger for Auditor {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> rusqlite::Result<()> {
        let mut insert = db.prepare("INSERT INTO audit(sequence) VALUES(?1)")?;
        for change in batch {
            insert.execute([change.sequence as i64])?;
        }
        Ok(())
    }
}

struct Refuser;

impl BulkTrigger for Refuser {
    fn on_batch(&mut self, _db: &Connection, _batch: &[RowChange]) -> rusqlite::Result<()> {
        Err(rusqlite::Error::ModuleError("the consumer refused".into()))
    }
}

fn orders() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE orders(id INTEGER PRIMARY KEY, amount INTEGER)")
        .unwrap();
    db
}

fn sequences(batch: &[RowChange]) -> Vec<u64> {
    batch.iter().map(|change| change.sequence).collect()
}

fn signs(batch: &[RowChange]) -> Vec<Sign> {
    batch.iter().map(|change| change.sign).collect()
}

fn delta_rows(db: &Connection, name: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM \"{name}_delta\""), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn one_statement_with_many_rows_draws_one_batch() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch("BEGIN; INSERT INTO orders VALUES(1,10),(2,20),(3,30); COMMIT;")
        .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 3);
    assert_eq!(sequences(&batch), vec![0, 1, 2]);
    assert_eq!(batch[0].table, "orders");
    assert_eq!(batch[2].values, vec![Value::Integer(3), Value::Integer(30)]);
}

#[test]
fn three_statements_draw_one_batch_in_statement_order() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch(
        "BEGIN;
         INSERT INTO orders VALUES(1,10),(2,20);
         DELETE FROM orders WHERE id=1;
         UPDATE orders SET amount=99 WHERE id=2;
         COMMIT;",
    )
    .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 5);
    assert_eq!(
        signs(&batch),
        vec![
            Sign::Insert,
            Sign::Insert,
            Sign::Delete,
            Sign::Delete,
            Sign::Insert
        ]
    );
    assert_eq!(batch[3].values, vec![Value::Integer(2), Value::Integer(20)]);
    assert_eq!(batch[4].values, vec![Value::Integer(2), Value::Integer(99)]);
}

#[test]
fn an_autocommit_statement_draws_one_batch() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch("INSERT INTO orders VALUES(1,10)").unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 1);
    assert_eq!(sequences(&batch), vec![0]);
}

#[test]
fn a_rolled_back_savepoint_drops_its_rows_and_renumbers() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch(
        "BEGIN;
         INSERT INTO orders VALUES(1,10),(2,20);
         SAVEPOINT inner_;
         INSERT INTO orders VALUES(3,30),(4,40);
         ROLLBACK TO inner_;
         INSERT INTO orders VALUES(5,50);
         COMMIT;",
    )
    .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 3);
    assert_eq!(sequences(&batch), vec![0, 1, 2]);
    assert_eq!(batch[2].values, vec![Value::Integer(5), Value::Integer(50)]);
}

#[test]
fn a_released_savepoint_keeps_its_rows() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch(
        "BEGIN;
         INSERT INTO orders VALUES(1,10),(2,20);
         SAVEPOINT inner_;
         INSERT INTO orders VALUES(3,30),(4,40);
         RELEASE inner_;
         INSERT INTO orders VALUES(5,50);
         COMMIT;",
    )
    .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 5);
    assert_eq!(sequences(&batch), vec![0, 1, 2, 3, 4]);
}

#[test]
fn a_savepoint_older_than_every_row_rolls_back_without_an_error() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch(
        "BEGIN;
         SAVEPOINT outer_;
         INSERT INTO orders VALUES(1,10),(2,20);
         ROLLBACK TO outer_;
         COMMIT;",
    )
    .unwrap();
    assert_eq!(recorder.batches().len(), 0);
    assert_eq!(delta_rows(&db, "collector"), 0);
}

#[test]
fn a_rolled_back_transaction_draws_no_batch() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch("BEGIN; INSERT INTO orders VALUES(1,10),(2,20),(3,30); ROLLBACK;")
        .unwrap();
    assert_eq!(recorder.batches().len(), 0);
    assert_eq!(delta_rows(&db, "collector"), 0);
}

#[test]
fn rows_past_the_memory_cap_come_back_in_sequence_order() {
    let db = orders();
    let recorder = Recorder::default();
    Watch::new("collector")
        .tables(&["orders"])
        .staged_rows(2)
        .install(&db, recorder.clone())
        .unwrap();
    db.execute_batch("INSERT INTO orders VALUES(1,10),(2,20),(3,30),(4,40),(5,50)")
        .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 5);
    assert_eq!(sequences(&batch), vec![0, 1, 2, 3, 4]);
    assert_eq!(
        batch[4].values,
        vec![Value::Integer(5), Value::Integer(50)]
    );
    assert_eq!(delta_rows(&db, "collector"), 0);
}

#[test]
fn a_rollback_to_after_a_spill_keeps_only_the_surviving_rows() {
    let db = orders();
    let recorder = Recorder::default();
    Watch::new("collector")
        .tables(&["orders"])
        .staged_rows(2)
        .install(&db, recorder.clone())
        .unwrap();
    db.execute_batch(
        "BEGIN;
         INSERT INTO orders VALUES(1,10),(2,20),(3,30);
         SAVEPOINT inner_;
         INSERT INTO orders VALUES(4,40),(5,50),(6,60);
         ROLLBACK TO inner_;
         COMMIT;",
    )
    .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 3);
    assert_eq!(sequences(&batch), vec![0, 1, 2]);
    assert_eq!(
        batch.iter().map(|c| c.values[0].clone()).collect::<Vec<_>>(),
        vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]
    );
    assert_eq!(delta_rows(&db, "collector"), 0);
}

#[test]
fn a_rows_original_types_survive_the_round_trip() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE motley(a,b,c,d,e)").unwrap();
    let recorder = Recorder::default();
    watch(&db, "collector", &["motley"], recorder.clone()).unwrap();
    db.execute_batch("INSERT INTO motley VALUES(1, 1.0, NULL, x'00', 'a')")
        .unwrap();
    let batch = recorder.only();
    assert_eq!(
        batch[0].values,
        vec![
            Value::Integer(1),
            Value::Real(1.0),
            Value::Null,
            Value::Blob(vec![0]),
            Value::Text("a".to_string()),
        ]
    );
}

#[test]
fn a_rows_original_types_survive_a_spill() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE motley(a,b,c,d,e)").unwrap();
    let recorder = Recorder::default();
    Watch::new("collector")
        .tables(&["motley"])
        .staged_rows(1)
        .install(&db, recorder.clone())
        .unwrap();
    db.execute_batch(
        "INSERT INTO motley VALUES(0,0,0,0,0),(1, 1.0, NULL, x'00', 'a')",
    )
    .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[1].values,
        vec![
            Value::Integer(1),
            Value::Real(1.0),
            Value::Null,
            Value::Blob(vec![0]),
            Value::Text("a".to_string()),
        ]
    );
}

#[test]
fn construction_draws_no_batch_and_zeroes_the_counts() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    assert_eq!(recorder.batches().len(), 0);
    assert_eq!(counts(&db, "collector"), Some(Counts::default()));
}

#[test]
fn three_statements_over_seven_rows_draw_the_documented_callbacks() {
    let db = orders();
    Watch::new("collector")
        .tables(&["orders"])
        .install(&db, Recorder::default())
        .unwrap();
    db.execute_batch(
        "BEGIN;
         INSERT INTO orders VALUES(1,10),(2,20),(3,30);
         INSERT INTO orders VALUES(4,40),(5,50);
         INSERT INTO orders VALUES(6,60),(7,70);
         COMMIT;",
    )
    .unwrap();
    assert_eq!(
        counts(&db, "collector"),
        Some(Counts {
            begin: 1,
            savepoint: 3,
            release: 3,
            rollback_to: 0,
            update: 7,
            sync: 1,
            commit: 1,
            rollback: 0,
        })
    );
}

#[test]
fn on_batch_may_write_ordinary_tables() {
    let db = orders();
    db.execute_batch("CREATE TABLE audit(sequence INTEGER)")
        .unwrap();
    watch(&db, "collector", &["orders"], Auditor).unwrap();
    db.execute_batch("BEGIN; INSERT INTO orders VALUES(1,10),(2,20); COMMIT;")
        .unwrap();
    let audited: Vec<i64> = db
        .prepare("SELECT sequence FROM audit ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(audited, vec![0, 1]);
}

#[test]
fn an_error_from_on_batch_fails_the_commit_and_keeps_the_tables_unchanged() {
    let db = orders();
    watch(&db, "collector", &["orders"], Refuser).unwrap();
    let refused = db
        .execute_batch("BEGIN; INSERT INTO orders VALUES(1,10),(2,20); COMMIT;")
        .unwrap_err();
    assert!(
        refused.to_string().contains("the consumer refused"),
        "{refused}"
    );
    let remaining: i64 = db
        .query_row("SELECT COUNT(*) FROM orders", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(delta_rows(&db, "collector"), 0);
}

/// xUpdate cannot tell a trigger body from a hand-written INSERT, so the guard
/// is the source name and the sign, not the writer.
#[test]
fn a_hand_written_insert_is_read_as_a_change_and_a_bad_one_is_refused() {
    let db = orders();
    let recorder = Recorder::default();
    watch(&db, "collector", &["orders"], recorder.clone()).unwrap();
    db.execute_batch("INSERT INTO collector(__source,__sign) VALUES('orders',1)")
        .unwrap();
    let batch = recorder.only();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].values, vec![Value::Null, Value::Null]);

    let unwatched = db
        .execute_batch("INSERT INTO collector(__source,__sign) VALUES('nothing',1)")
        .unwrap_err();
    assert!(
        unwatched.to_string().contains("is not watched here"),
        "{unwatched}"
    );

    let sign = db
        .execute_batch("INSERT INTO collector(__source,__sign) VALUES('orders',7)")
        .unwrap_err();
    assert!(sign.to_string().contains("wrote sign 7"), "{sign}");
}

/// The same input as `a_rolled_back_savepoint_drops_its_rows_and_renumbers`,
/// driven through the exported state machine with no virtual table in sight.
#[test]
fn the_state_machine_alone_survives_a_rolled_back_savepoint() {
    let db = Connection::open_in_memory().unwrap();
    let mut collector = Collector::new("embedded", 2);
    collector.create_shadow(&db).unwrap();

    let order = |id: i64, amount: i64| {
        RowChange::new(
            "orders",
            Sign::Insert,
            vec![Value::Integer(id), Value::Integer(amount)],
        )
    };

    collector.begin();
    collector.savepoint(0);
    collector.update(&db, order(1, 10)).unwrap();
    collector.update(&db, order(2, 20)).unwrap();
    collector.release(0);

    collector.savepoint(0);
    collector.savepoint(1);
    collector.update(&db, order(3, 30)).unwrap();
    collector.update(&db, order(4, 40)).unwrap();
    collector.release(1);
    collector.rollback_to(0);

    collector.savepoint(1);
    collector.update(&db, order(5, 50)).unwrap();
    collector.release(1);

    let batch = collector.drain(&db).unwrap();
    collector.commit();
    assert_eq!(batch.len(), 3);
    assert_eq!(sequences(&batch), vec![0, 1, 2]);
    assert_eq!(batch[2].values, vec![Value::Integer(5), Value::Integer(50)]);
    assert_eq!(delta_rows(&db, "embedded"), 0);
    assert_eq!(collector.counts().update, 5);
    assert_eq!(collector.counts().rollback_to, 1);
}

fn embedded() -> (Connection, Collector) {
    let db = Connection::open_in_memory().unwrap();
    let collector = Collector::new("embedded", 2);
    collector.create_shadow(&db).unwrap();
    (db, collector)
}

fn order(id: i64) -> RowChange {
    RowChange::new(
        "orders",
        Sign::Insert,
        vec![Value::Integer(id), Value::Integer(id * 10)],
    )
}

fn ids(batch: &[RowChange]) -> Vec<Value> {
    batch.iter().map(|change| change.values[0].clone()).collect()
}

#[test]
fn a_drain_inside_a_savepoint_hands_the_older_rows_back_on_rollback_to() {
    let (db, mut collector) = embedded();
    collector.begin();
    collector.update(&db, order(1)).unwrap();
    collector.savepoint(0);
    collector.update(&db, order(2)).unwrap();
    collector.update(&db, order(3)).unwrap();
    let first = collector.drain(&db).unwrap();
    assert_eq!(sequences(&first), vec![0, 1, 2]);
    collector.rollback_to(0);

    let second = collector.drain(&db).unwrap();
    collector.release(0);
    collector.commit();
    assert_eq!(ids(&second), vec![Value::Integer(1)]);
    assert_eq!(sequences(&second), vec![0]);
}

#[test]
fn a_drain_before_a_savepoint_is_not_unwound_by_rollback_to() {
    let (db, mut collector) = embedded();
    collector.begin();
    collector.update(&db, order(1)).unwrap();
    let first = collector.drain(&db).unwrap();
    assert_eq!(first.len(), 1);
    collector.savepoint(0);
    collector.update(&db, order(2)).unwrap();
    collector.rollback_to(0);

    let second = collector.drain(&db).unwrap();
    collector.release(0);
    collector.commit();
    assert!(second.is_empty());
}

#[test]
fn a_drain_inside_the_inner_savepoint_restages_only_rows_before_the_outer() {
    let (db, mut collector) = embedded();
    collector.begin();
    collector.update(&db, order(1)).unwrap();
    collector.savepoint(0);
    collector.update(&db, order(2)).unwrap();
    collector.savepoint(1);
    collector.update(&db, order(3)).unwrap();
    let first = collector.drain(&db).unwrap();
    assert_eq!(sequences(&first), vec![0, 1, 2]);
    collector.update(&db, order(4)).unwrap();
    collector.rollback_to(0);

    let second = collector.drain(&db).unwrap();
    assert_eq!(ids(&second), vec![Value::Integer(1)]);
    assert_eq!(sequences(&second), vec![0]);

    collector.update(&db, order(5)).unwrap();
    let third = collector.drain(&db).unwrap();
    collector.release(0);
    collector.commit();
    assert_eq!(ids(&third), vec![Value::Integer(5)]);
    assert_eq!(sequences(&third), vec![1]);
}

#[test]
fn a_delete_against_the_collector_is_refused() {
    let db = orders();
    watch(&db, "collector", &["orders"], Recorder::default()).unwrap();
    let refused = db.execute_batch("DELETE FROM collector").unwrap_err();
    assert!(refused.to_string().contains("triggers only"), "{refused}");
}
