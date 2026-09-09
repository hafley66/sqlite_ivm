#![cfg(not(feature = "extension"))]
use rusqlite::{functions::FunctionFlags, types::Value, Connection, Result};
#[path = "support/0_database.rs"]
mod database;
use database::register;
fn open(path: &std::path::Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    register(&db)?;
    db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;PRAGMA foreign_keys=ON")?;
    Ok(db)
}
fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut s = db.prepare(sql)?;
    let n = s.column_count();
    let mut rows = s
        .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
        .collect::<Result<Vec<Vec<Value>>>>()?;
    rows.sort_by_key(|r| format!("{r:?}"));
    Ok(rows)
}

#[test]
fn wal_snapshots_writer_contention_and_failed_maintenance_are_atomic() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "ivm-transaction-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writer = open(&path)?;
    let query="SELECT a.k,COUNT(*) AS n,SUM(a.v) AS s,MIN(b.w) AS lo FROM a LEFT JOIN b ON a.k=b.k GROUP BY a.k";
    writer.execute_batch(&format!("CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER,w INTEGER);INSERT INTO a VALUES(1,1,7),(2,2,3);INSERT INTO b VALUES(1,1,9);CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
    let reader = open(&path)?;
    let contender = open(&path)?;
    contender.busy_timeout(std::time::Duration::ZERO)?;
    let original = rows(&reader, "SELECT * FROM result")?;
    reader.execute_batch("BEGIN")?;
    assert_eq!(rows(&reader, query)?, original);
    writer.execute_batch(
        "BEGIN IMMEDIATE;UPDATE a SET k=2,v=10 WHERE id=1;INSERT INTO b VALUES(2,2,5)",
    )?;
    assert_eq!(
        rows(&writer, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    assert_eq!(rows(&reader, "SELECT * FROM result")?, original);
    let error = contender.execute_batch("UPDATE a SET v=99").unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    writer.execute_batch("COMMIT")?;
    assert_eq!(rows(&reader, "SELECT * FROM result")?, original);
    assert_eq!(rows(&reader, query)?, original);
    reader.execute_batch("COMMIT")?;
    assert_eq!(
        rows(&reader, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    writer.execute_batch("CREATE TEMP TRIGGER fail_result BEFORE INSERT ON main.result_state BEGIN SELECT RAISE(ABORT,'injected result write failure');END")?;
    let before = rows(&writer, "SELECT * FROM a")?;
    assert!(writer.execute_batch("UPDATE a SET v=11").is_err());
    assert_eq!(rows(&writer, "SELECT * FROM a")?, before);
    assert_eq!(
        rows(&writer, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    writer.execute_batch("DROP TRIGGER fail_result;UPDATE a SET v=11")?;
    assert_eq!(
        rows(&writer, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    // SQLite protects module-declared shadow tables from external writes while
    // the module is registered. Use an unregistered connection to simulate
    // on-disk corruption, then verify that the ordinary query detects it.
    drop(reader);
    drop(contender);
    drop(writer);
    let raw = Connection::open(&path)?;
    raw.execute_batch("UPDATE result_state SET c2=1234567")?;
    drop(raw);
    let writer = open(&path)?;
    assert_ne!(
        rows(&writer, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    // A no-op source update runs the normal maintenance path and repairs the
    // deliberately corrupted aggregate state.
    writer.execute_batch("UPDATE a SET v=v")?;
    assert_eq!(
        rows(&writer, "SELECT * FROM result")?,
        rows(&writer, query)?
    );
    drop(writer);
    let reopened = open(&path)?;
    assert_eq!(
        rows(&reopened, "SELECT * FROM result")?,
        rows(&reopened, query)?
    );
    drop(reopened);
    std::fs::remove_file(path).unwrap();
    Ok(())
}

#[test]
fn cascades_generated_values_and_user_trigger_writes_compose() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;PRAGMA foreign_keys=ON;
        CREATE TABLE farms(id INTEGER PRIMARY KEY,region TEXT);
        CREATE TABLE lots(id INTEGER PRIMARY KEY,farm INTEGER REFERENCES farms(id) ON UPDATE CASCADE ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,crates INTEGER,price INTEGER,total INTEGER GENERATED ALWAYS AS(crates*price) STORED);
        CREATE TABLE audit(id INTEGER PRIMARY KEY,lot INTEGER,amount INTEGER);
        CREATE TRIGGER log_sale AFTER INSERT ON lots BEGIN INSERT INTO audit(lot,amount) VALUES(NEW.id,NEW.total);END;
        CREATE TRIGGER log_price AFTER UPDATE OF price,crates ON lots BEGIN UPDATE audit SET amount=NEW.total WHERE lot=NEW.id;END;
        CREATE TRIGGER log_delete AFTER DELETE ON lots BEGIN DELETE FROM audit WHERE lot=OLD.id;END;")?;
    let queries=["SELECT farms.region,COUNT(lots.id) AS n,SUM(lots.total) AS dollars FROM farms LEFT JOIN lots ON farms.id=lots.farm GROUP BY farms.region",
        "SELECT lot,COUNT(*) AS n,SUM(amount) AS amount FROM audit GROUP BY lot",
        "SELECT lots.id,audit.amount FROM lots JOIN audit ON lots.id=audit.lot"];
    for (i, q) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{q}')"
        ))?;
    }
    let verify = || -> Result<()> {
        for (i, q) in queries.iter().enumerate() {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM v{i}"))?,
                rows(&db, q)?,
                "{q}"
            );
        }
        Ok(())
    };
    for sql in [
        "INSERT INTO farms VALUES(1,'north'),(2,'south')",
        "INSERT INTO lots(id,farm,crates,price) VALUES(1,1,7,20),(2,1,3,10),(3,2,2,8)",
        "UPDATE lots SET price=25 WHERE id=1",
        "UPDATE farms SET id=3 WHERE id=1",
        "BEGIN;DELETE FROM farms WHERE id=3",
        "ROLLBACK",
        "DELETE FROM farms WHERE id=3",
        "BEGIN;UPDATE lots SET farm=99",
    ] {
        db.execute_batch(sql)?;
        verify()?;
    }
    assert!(db.execute_batch("COMMIT").is_err());
    verify()?;
    db.execute_batch("ROLLBACK;DELETE FROM farms")?;
    verify()?;
    Ok(())
}

#[test]
fn deterministic_registered_scalars_and_rejected_volatile_functions() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.create_scalar_function(
        "farm_fee",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<i64>(0)? * 3 + 2),
    )?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,v INTEGER);
        CREATE VIRTUAL TABLE fees USING sqlite_ivm('SELECT id,farm_fee(coalesce(v,0)) AS fee,json_extract(json_object(''crates'',v),''$.crates'') AS crates FROM farms')")?;
    let query="SELECT id,farm_fee(coalesce(v,0)),json_extract(json_object('crates',v),'$.crates') FROM farms";
    for sql in [
        "INSERT INTO farms VALUES(1,7),(2,NULL)",
        "UPDATE farms SET v=3",
        "DELETE FROM farms WHERE id=1",
        "DELETE FROM farms",
    ] {
        db.execute_batch(sql)?;
        assert_eq!(rows(&db, "SELECT * FROM fees")?, rows(&db, query)?);
    }
    for q in [
        "SELECT random() AS n FROM farms",
        "SELECT datetime('now') AS now FROM farms",
        "SELECT group_concat(v) AS all_values FROM farms",
    ] {
        assert!(db
            .execute_batch(&format!(
                "CREATE VIRTUAL TABLE bad USING sqlite_ivm('{}')",
                q.replace('\'', "''")
            ))
            .is_err());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'bad%'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
    }
    Ok(())
}
