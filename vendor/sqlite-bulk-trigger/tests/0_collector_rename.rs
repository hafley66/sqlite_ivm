use rusqlite::{types::Value, Connection, Result};
use sqlite_bulk_trigger::{Collector, RowChange, Sign};

#[test]
fn renamed_shadow_preserves_memory_spill_and_savepoint_replay() -> Result<()> {
    let db = Connection::open_in_memory()?;
    let mut collector = Collector::new("old", 1).staged_rows(1);
    collector.create_shadow(&db)?;
    db.execute_batch("BEGIN")?;
    collector.begin();
    for value in [10,20] {
        collector.update(&db, RowChange::new("source", Sign::Insert, vec![Value::Integer(value)]))?;
    }
    db.execute_batch("SAVEPOINT rename; ALTER TABLE old_delta RENAME TO new_delta")?;
    collector.savepoint(0);
    collector.rebind_shadow("new");
    let batch = collector.drain(&db)?;
    assert_eq!(batch.iter().map(|r| (r.sequence,r.values.clone())).collect::<Vec<_>>(), vec![
        (0,vec![Value::Integer(10)]),(1,vec![Value::Integer(20)])
    ]);
    db.execute_batch("ROLLBACK TO rename")?;
    collector.rollback_to(0);
    collector.rebind_shadow("old");
    assert_eq!(collector.drain(&db)?, batch);
    db.execute_batch("RELEASE rename; COMMIT")?;
    collector.release(0);
    collector.commit();
    assert_eq!(db.query_row("SELECT count(*) FROM old_delta", [], |r|r.get::<_,i64>(0))?,0);
    Ok(())
}
