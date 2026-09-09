#![cfg(not(feature = "extension"))]
use rusqlite::{config::DbConfig, types::Value, Connection, Result};
use sqlite_ivm::extension::register;

const QUERY:&str="SELECT l.farmer AS farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars FROM lots l JOIN prices p ON l.farm=p.farm AND l.crop=p.crop WHERE l.crates>=5 GROUP BY l.farmer";
fn database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
        CREATE TABLE lots(id INTEGER PRIMARY KEY,farm INTEGER NOT NULL,crop INTEGER NOT NULL,farmer INTEGER NOT NULL,crates INTEGER NOT NULL);
        CREATE TABLE prices(id INTEGER PRIMARY KEY,farm INTEGER NOT NULL,crop INTEGER NOT NULL,dollars INTEGER NOT NULL);
        INSERT INTO lots VALUES(1,1,10,101,7),(2,1,10,102,6),(3,2,10,-3,9);
        INSERT INTO prices VALUES(1,1,10,20),(2,2,10,30);")?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE earnings USING sqlite_ivm('{QUERY}')"
    ))?;
    Ok(db)
}
fn verify(db: &Connection, name: &str) -> Result<()> {
    let rows = |sql: &str| -> Result<Vec<(i64, i64, i64)>> {
        db.prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
    };
    assert_eq!(
        rows(&format!("SELECT * FROM {name} ORDER BY 1"))?,
        rows(&format!("{QUERY} ORDER BY 1"))?
    );
    for suffix in ["delta"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {name}_{suffix}"), [], |r| {
                r.get::<_, i64>(0)
            })?,
            0
        );
    }
    Ok(())
}
fn schema(db: &Connection) -> Result<Vec<(String, String, String)>> {
    db.prepare("SELECT type,name,coalesce(sql,'') FROM main.sqlite_schema ORDER BY type,name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}
#[test]
fn ddl_rename_preserves_state_without_writes_and_drop_preserves_sources() -> Result<()> {
    let db = database()?;
    let roots = |name: &str| -> Result<Vec<i64>> {
        db.prepare("SELECT s.rootpage FROM sqlite_schema s JOIN __ivm_objects o ON o.object_name=s.name WHERE o.view_name=?1 AND o.object_type='index' ORDER BY o.object_name")?
            .query_map([name],|r|r.get(0))?.collect()
    };
    let original_roots = roots("earnings")?;
    db.execute_batch("CREATE TABLE writes(g INTEGER NOT NULL);
        CREATE TRIGGER observed_update AFTER UPDATE ON earnings_state BEGIN INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER observed_insert AFTER INSERT ON earnings_state BEGIN INSERT INTO writes VALUES(NEW.g); END;
        CREATE TRIGGER observed_delete AFTER DELETE ON earnings_state BEGIN INSERT INTO writes VALUES(OLD.g); END;
        ALTER TABLE earnings RENAME TO income;")?;
    verify(&db, "income")?;
    assert_eq!(roots("income")?, original_roots);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM writes", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='earnings' OR name GLOB 'earnings_*' OR (type='trigger' AND name GLOB '__ivm_earnings_*')",[],|r|r.get::<_,i64>(0))?,0);
    db.execute_batch("UPDATE lots SET crates=3 WHERE id=1; UPDATE prices SET dollars=25 WHERE id=1; INSERT INTO lots VALUES(4,1,10,101,8); DELETE FROM lots WHERE id=2")?;
    verify(&db, "income")?;
    // The original public name remains reusable while income owns its old indexes.
    db.execute_batch(&format!("CREATE VIRTUAL TABLE earnings USING sqlite_ivm('{QUERY}'); UPDATE prices SET dollars=26 WHERE id=1"))?;
    verify(&db, "earnings")?;
    verify(&db, "income")?;
    db.execute_batch("DROP TABLE earnings")?;
    db.execute_batch("DROP TABLE income")?;
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM lots", [], |r| r.get::<_, i64>(0))?,
        3
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name='income' OR name GLOB 'income_*' OR name GLOB '__ivm_income_*'",[],|r|r.get::<_,i64>(0))?,0);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM __ivm_objects", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}
#[test]
fn ddl_rename_rollback_savepoints_and_failure_restore_usable_names() -> Result<()> {
    let db = database()?;
    let before = schema(&db)?;
    db.execute_batch(
        "BEGIN; ALTER TABLE earnings RENAME TO income; UPDATE lots SET crates=8 WHERE id=1",
    )?;
    verify(&db, "income")?;
    db.execute_batch("ROLLBACK")?;
    assert_eq!(schema(&db)?, before);
    db.execute_batch("UPDATE prices SET dollars=21 WHERE id=1;
        SAVEPOINT outer_scope; ALTER TABLE earnings RENAME TO income;
        SAVEPOINT inner_scope; ALTER TABLE income RENAME TO receipts; UPDATE lots SET crates=9 WHERE id=2;")?;
    verify(&db, "receipts")?;
    db.execute_batch(
        "ROLLBACK TO inner_scope; RELEASE inner_scope; UPDATE lots SET crates=10 WHERE id=1",
    )?;
    verify(&db, "income")?;
    db.execute_batch(
        "ROLLBACK TO outer_scope; RELEASE outer_scope; UPDATE lots SET crates=11 WHERE id=1",
    )?;
    verify(&db, "earnings")?;
    assert_eq!(schema(&db)?, before);
    db.execute_batch("CREATE TABLE blocked_state(x); BEGIN; UPDATE lots SET crates=12 WHERE id=1")?;
    let collision_schema = schema(&db)?;
    assert!(db
        .execute_batch("ALTER TABLE earnings RENAME TO blocked")
        .is_err());
    assert!(!db.is_autocommit());
    assert_eq!(schema(&db)?, collision_schema);
    verify(&db, "earnings")?;
    db.execute_batch("CREATE TEMP TRIGGER reject_rename BEFORE UPDATE ON main.__ivm_views BEGIN SELECT RAISE(ABORT,'injected rename failure'); END")?;
    let failure = db
        .execute_batch("ALTER TABLE earnings RENAME TO income")
        .unwrap_err();
    assert!(
        failure.to_string().contains("injected rename failure"),
        "{failure}"
    );
    assert_eq!(schema(&db)?, collision_schema);
    db.execute_batch(
        "DROP TRIGGER temp.reject_rename; UPDATE prices SET dollars=22 WHERE id=1; COMMIT",
    )?;
    verify(&db, "earnings")?;
    Ok(())
}
#[test]
fn defensive_shadow_protection_allows_source_dml_and_native_lifecycle() -> Result<()> {
    let db = database()?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    let types=db.prepare("SELECT name,type FROM pragma_table_list WHERE name='earnings' OR name GLOB 'earnings_*' ORDER BY name")?
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>>>()?;
    assert_eq!(
        types,
        vec![
            ("earnings".into(), "virtual".into()),
            ("earnings_delta".into(), "shadow".into()),
            ("earnings_state".into(), "shadow".into())
        ]
    );
    for sql in [
        "DELETE FROM earnings_state",
        "UPDATE earnings_state SET s=0",
        "DROP TABLE earnings_delta",
        "INSERT INTO earnings VALUES(1,2,3)",
        "UPDATE earnings SET dollars=0",
        "DELETE FROM earnings",
    ] {
        assert!(db.execute_batch(sql).is_err(), "{sql}");
    }
    db.execute_batch("UPDATE lots SET crates=4 WHERE id=1; INSERT INTO lots VALUES(4,1,10,101,8); DELETE FROM prices WHERE id=2")?;
    verify(&db, "earnings")?;
    db.execute_batch(
        "ALTER TABLE earnings RENAME TO income; UPDATE prices SET dollars=23 WHERE id=1",
    )?;
    verify(&db, "income")?;
    db.execute_batch("DROP TABLE income")?;
    Ok(())
}
#[test]
fn indexed_cursors_preserve_output_order_affinity_and_simultaneous_reads() -> Result<()> {
    let db = database()?;
    for value in [
        Value::Null,
        Value::Integer(101),
        Value::Integer(-3),
        Value::Integer(999),
        Value::Real(101.0),
        Value::Real(101.5),
        Value::Text("101".into()),
        Value::Text("no".into()),
        Value::Blob(vec![49, 48, 49]),
    ] {
        for key in ["farmer", "rowid"] {
            let actual = db
                .prepare(&format!(
                    "SELECT farmer,n,dollars FROM earnings WHERE {key}=?1"
                ))?
                .query_map([&value], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>>>()?;
            let expected = db
                .prepare("SELECT g,n,s FROM earnings_state WHERE g=?1")?
                .query_map([&value], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(actual, expected, "{key} {value:?}");
        }
    }
    let plan: String = db.query_row(
        "EXPLAIN QUERY PLAN SELECT * FROM earnings WHERE farmer=101",
        [],
        |r| r.get(3),
    )?;
    assert!(plan.contains("group_key"), "{plan}");
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM earnings a JOIN earnings b ON a.farmer=b.farmer",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        3
    );
    db.execute_batch("CREATE VIRTUAL TABLE reordered USING sqlite_ivm('SELECT SUM(crates) AS total,farmer AS owner,COUNT(*) AS count FROM lots GROUP BY farmer')")?;
    assert_eq!(
        db.query_row("SELECT * FROM reordered WHERE owner=101", [], |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?
        )))?,
        (7, 101, 1)
    );
    Ok(())
}

#[test]
fn catalog_identity_survives_vacuum_and_rename_rejects_modified_hooks() -> Result<()> {
    let db = database()?;
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE income USING sqlite_ivm('{QUERY}'); DROP TABLE earnings; VACUUM"
    ))?;
    assert_eq!(
        db.query_row("SELECT id FROM __ivm_views WHERE name='income'", [], |r| {
            r.get::<_, i64>(0)
        })?,
        2
    );
    db.execute_batch("UPDATE lots SET crates=8 WHERE id=1")?;
    verify(&db, "income")?;
    let trigger: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='__ivm_income_0_delete'",
        [],
        |r| r.get(0),
    )?;
    db.execute_batch(
        "DROP TRIGGER __ivm_income_0_delete;
        CREATE TRIGGER __ivm_income_0_delete AFTER DELETE ON lots BEGIN SELECT 1; END",
    )?;
    let before = schema(&db)?;
    assert!(db
        .execute_batch("ALTER TABLE income RENAME TO receipts")
        .is_err());
    assert_eq!(schema(&db)?, before);
    db.execute_batch("DROP TRIGGER __ivm_income_0_delete")?;
    db.execute_batch(&trigger)?;
    db.execute_batch("ALTER TABLE income RENAME TO receipts; DELETE FROM lots WHERE id=1")?;
    verify(&db, "receipts")?;
    Ok(())
}
