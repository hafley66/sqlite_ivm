#![cfg(not(feature = "extension"))]
use rusqlite::{types::Value, Connection, Result};
use sqlite_ivm::extension::register;
#[test]
fn shared_circuit_states() -> Result<()> {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/0_shared.json")).unwrap();
    let mut failures = vec![];
    for fixture in fixtures.as_array().unwrap() {
        let family = fixture["circuit"].as_str().unwrap();
        let result = (|| -> Result<()> {
            let db = Connection::open_in_memory()?;
            register(&db)?;
            db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);")?;
            let sql = fixture["query"].as_str().unwrap();
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
                sql.replace('\'', "''")
            ))?;
            for state in fixture["states"].as_array().unwrap() {
                db.execute_batch(&format!(
                    "BEGIN;{} COMMIT;",
                    state["mutation_sql"].as_str().unwrap()
                ))?;
                let mut statement = db.prepare("SELECT * FROM result")?;
                let n = statement.column_count();
                let mut actual = statement
                    .query_map([], |r| {
                        (0..n)
                            .map(|i| r.get::<_, i64>(i))
                            .collect::<Result<Vec<_>>>()
                    })?
                    .collect::<Result<Vec<_>>>()?;
                actual.sort();
                let expected = state["expected"]["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        r.as_array()
                            .unwrap()
                            .iter()
                            .map(|n| n.as_i64().unwrap())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "{family} {}", state["name"]);
            }
            db.execute_batch("ALTER TABLE result RENAME TO renamed; BEGIN;DELETE FROM a;ROLLBACK;DROP TABLE renamed")?;
            Ok(())
        })();
        if let Err(e) = result {
            failures.push(format!("{family}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
#[test]
fn nullable_text_composite_groups_outer_join_and_rollback() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,region TEXT,crop TEXT,price INTEGER);CREATE TABLE lots(id INTEGER PRIMARY KEY,farm INTEGER,crates INTEGER);")?;
    let queries=["SELECT region,crop,COUNT(*) AS n,SUM(price) AS s,AVG(price) AS mean,MIN(price) AS low,MAX(price) AS high FROM farms GROUP BY region,crop",
        "SELECT farms.region,lots.crates FROM farms LEFT JOIN lots ON farms.id=lots.farm"];
    for (i, q) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{q}')"
        ))?;
    }
    let rows = |sql: &str| -> Result<Vec<Vec<Value>>> {
        let mut s = db.prepare(sql)?;
        let n = s.column_count();
        let mut r = s
            .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
            .collect::<Result<Vec<Vec<Value>>>>()?;
        r.sort_by_key(|r| format!("{r:?}"));
        Ok(r)
    };
    for mutation in ["INSERT INTO farms VALUES(1,'north','apples',NULL),(2,'north','apples',20),(3,'south','pears',7)","INSERT INTO lots VALUES(1,1,4),(2,1,8)","UPDATE farms SET price=10 WHERE id=1","BEGIN;DELETE FROM lots;UPDATE farms SET crop='pears'","ROLLBACK","UPDATE lots SET farm=2 WHERE id=1","DELETE FROM farms WHERE id=2","DELETE FROM lots","DELETE FROM farms"]{
        db.execute_batch(mutation)?;
        for(i,q)in queries.iter().enumerate(){assert_eq!(rows(&format!("SELECT * FROM v{i}"))?,rows(q)?,"{mutation}: {q}");}
    }
    Ok(())
}
#[test]
fn global_empty_aggregates_and_source_ddl() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,price INTEGER);
        CREATE VIRTUAL TABLE total USING sqlite_ivm('SELECT COUNT(*) AS n,COUNT(price) AS priced,SUM(price) AS amount,AVG(price) AS mean FROM farms');
        CREATE VIRTUAL TABLE nested USING sqlite_ivm('SELECT SUM(n) AS n FROM (SELECT COUNT(*) AS n FROM farms)');")?;
    let verify = || -> Result<()> {
        let actual: (i64, i64, Option<i64>, Option<f64>) =
            db.query_row("SELECT * FROM total", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
        let source: String = db.query_row(
            "SELECT table_name FROM __ivm_sources WHERE view_name='total'",
            [],
            |r| r.get(0),
        )?;
        let column: String = db.query_row(
            "SELECT column_name FROM __ivm_columns WHERE view_name='total' AND column_name!='id'",
            [],
            |r| r.get(0),
        )?;
        let expected = db.query_row(
            &format!("SELECT COUNT(*),COUNT({column}),SUM({column}),AVG({column}) FROM {source}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        assert_eq!(actual, expected);
        assert_eq!(
            db.query_row("SELECT n FROM nested", [], |r| r.get::<_, i64>(0))?,
            actual.0
        );
        Ok(())
    };
    verify()?;
    for sql in [
        "INSERT INTO farms VALUES(1,NULL),(2,8)",
        "UPDATE farms SET price=20 WHERE id=1",
        "DELETE FROM farms",
        "BEGIN;SELECT sqlite_ivm_rename_source('farms','growers')",
        "INSERT INTO growers VALUES(1,7)",
        "ROLLBACK",
        "SELECT sqlite_ivm_rename_source('farms','growers')",
        "INSERT INTO growers VALUES(1,9)",
        "SELECT sqlite_ivm_rename_column('growers','price','cost')",
        "UPDATE growers SET cost=11",
        "ALTER TABLE total RENAME TO moved;ALTER TABLE moved RENAME TO total",
    ] {
        db.execute_batch(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        verify()?;
    }
    assert!(db
        .execute_batch("SELECT sqlite_ivm_drop_source('growers',0)")
        .is_err());
    verify()?;
    db.execute_batch("BEGIN;SELECT sqlite_ivm_drop_source('growers',1);ROLLBACK")?;
    verify()?;
    db.execute_batch("SELECT sqlite_ivm_drop_source('growers',1)")?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM __ivm_views", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    Ok(())
}
#[test]
fn narrow_source_column_order_changes_are_transactional() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,g INTEGER,z INTEGER);
        INSERT INTO farms VALUES(1,4,7);CREATE VIRTUAL TABLE totals USING sqlite_ivm('SELECT g AS farm,COUNT(*) AS n,SUM(z) AS amount FROM farms GROUP BY g');
        SELECT sqlite_ivm_rename_column('farms','z','a');INSERT INTO farms VALUES(2,4,8);
        SELECT sqlite_ivm_rename_source('farms','growers');UPDATE growers SET a=10 WHERE id=1;")?;
    assert_eq!(
        db.query_row("SELECT * FROM totals", [], |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?
        )))?,
        (4, 2, 18)
    );
    db.execute_batch("BEGIN;SELECT sqlite_ivm_rename_column('growers','a','zzz');UPDATE growers SET zzz=100;ROLLBACK;UPDATE growers SET a=12 WHERE id=2;ALTER TABLE totals RENAME TO earnings;DROP TABLE earnings")?;
    Ok(())
}
#[test]
fn deterministic_relational_mutations_and_type_contract() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);")?;
    let queries=[
        "SELECT a.k AS k,b.v AS value FROM a LEFT JOIN b ON a.k=b.k",
        "SELECT k AS k,COUNT(v) AS n,AVG(v) AS mean,MIN(v) AS lo,MAX(v) AS hi,COUNT(DISTINCT v) AS uniq FROM a GROUP BY k",
        "SELECT k AS k,v AS value FROM a UNION SELECT k,v FROM b",
        "SELECT id AS id,v AS value FROM a ORDER BY v DESC,id ASC LIMIT 3",
        "SELECT id AS id,ROW_NUMBER() OVER(PARTITION BY k ORDER BY v,id) AS rank FROM a"
    ];
    for (i, q) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{q}')"
        ))?;
    }
    let rows = |sql: &str| -> Result<Vec<Vec<Value>>> {
        let mut s = db.prepare(sql)?;
        let n = s.column_count();
        let mut r = s
            .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
            .collect::<Result<Vec<Vec<Value>>>>()?;
        r.sort_by_key(|r| format!("{r:?}"));
        Ok(r)
    };
    let mut seed = 0x518a_007du64;
    for step in 0..160 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let table = if seed & 1 == 0 { "a" } else { "b" };
        let id = (seed >> 8) % 19;
        let key = if seed & 16 == 0 {
            "NULL".into()
        } else {
            ((seed >> 16) % 5).to_string()
        };
        let value = if seed & 32 == 0 {
            "NULL".into()
        } else {
            (((seed >> 24) % 21) as i64 - 10).to_string()
        };
        let sql=match (seed>>4)%3 {0=>format!("DELETE FROM {table} WHERE id={id}"),1=>format!("INSERT INTO {table} VALUES({id},{key},{value}) ON CONFLICT(id) DO UPDATE SET k=excluded.k,v=excluded.v"),_=>format!("INSERT OR REPLACE INTO {table} VALUES({id},{key},{value})")};
        if step % 17 == 0 {
            db.execute_batch("SAVEPOINT trial")?;
        }
        db.execute_batch(&sql)?;
        for (i, q) in queries.iter().enumerate() {
            assert_eq!(
                rows(&format!("SELECT * FROM v{i}"))?,
                rows(q)?,
                "step {step}: {sql}; {q}"
            );
        }
        if step % 17 == 0 {
            db.execute_batch("ROLLBACK TO trial; RELEASE trial")?;
        }
    }
    db.execute_batch("CREATE TABLE labels(id INTEGER PRIMARY KEY,label TEXT);INSERT INTO labels VALUES(1,'2'),(2,'10');CREATE VIRTUAL TABLE lexical USING sqlite_ivm('SELECT label FROM labels WHERE label < 3')")?;
    assert_eq!(
        rows("SELECT * FROM lexical")?,
        rows("SELECT label FROM labels WHERE label < 3")?
    );
    assert!(db
        .execute_batch("INSERT INTO a VALUES(100,1,'bad')")
        .is_err());
    db.execute_batch("CREATE VIRTUAL TABLE mixed_keys USING sqlite_ivm('SELECT a.id AS x,labels.id AS y FROM a JOIN labels ON a.k=labels.label')")?;
    assert_eq!(
        rows("SELECT * FROM mixed_keys")?,
        rows("SELECT a.id,labels.id FROM a JOIN labels ON a.k=labels.label")?
    );
    db.execute_batch("CREATE TABLE insensitive(id INTEGER PRIMARY KEY,s TEXT COLLATE NOCASE)")?;
    db.execute_batch("CREATE VIRTUAL TABLE insensitive_values USING sqlite_ivm('SELECT DISTINCT s FROM insensitive');INSERT INTO insensitive VALUES(1,'farm'),(2,'FARM')")?;
    assert_eq!(
        rows("SELECT lower(s) FROM insensitive_values")?,
        vec![vec![Value::Text("farm".into())]]
    );
    db.execute_batch("CREATE VIRTUAL TABLE distinct_top USING sqlite_ivm('SELECT DISTINCT k FROM a ORDER BY k LIMIT 2')")?;
    assert_eq!(
        rows("SELECT * FROM distinct_top")?,
        rows("SELECT DISTINCT k FROM a ORDER BY k LIMIT 2")?
    );
    Ok(())
}
#[test]
fn unsupported_clause_combinations_fail_before_install() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER)")?;
    for q in ["SELECT k,COUNT(*) AS n FROM a GROUP BY 9","SELECT id FROM a ORDER BY 8 LIMIT 3","SELECT id FROM a WHERE EXISTS(SELECT 1 FROM b WHERE b.k=a.k LIMIT 0)","WITH RECURSIVE r(n) AS(SELECT k FROM b UNION SELECT a.v FROM a LEFT JOIN r ON a.k=r.n) SELECT n FROM r"] {
        assert!(db.execute_batch(&format!("CREATE VIRTUAL TABLE bad USING sqlite_ivm('{q}')")).is_err(),"{q}");
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'bad%' OR name LIKE '__ivm_bad%'",[],|r|r.get::<_,i64>(0))?,0);
    }
    Ok(())
}
#[test]
fn relational_group_locality_and_ddl_preserve_btrees() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,region TEXT,price INTEGER);INSERT INTO farms VALUES(1,'north',20),(2,'north',30),(3,'south',80);
        CREATE VIRTUAL TABLE earnings USING sqlite_ivm('SELECT region,MIN(price) AS lo,MAX(price) AS hi FROM farms GROUP BY region');
        CREATE TABLE writes(region TEXT);
        CREATE TRIGGER watch_insert AFTER INSERT ON earnings_state BEGIN INSERT INTO writes VALUES(NEW.c0);END;
        CREATE TRIGGER watch_delete AFTER DELETE ON earnings_state BEGIN INSERT INTO writes VALUES(OLD.c0);END;
        UPDATE farms SET price=25 WHERE id=1;")?;
    let written = db
        .prepare("SELECT DISTINCT region FROM writes")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(written, vec!["north"]);
    // Remove observer triggers before testing source ALTER schema validation.
    db.execute_batch("DROP TRIGGER watch_insert;DROP TRIGGER watch_delete;DELETE FROM writes")?;
    let physical = || -> Result<Vec<(String, i64)>> {
        db.prepare("SELECT o.object_type,s.rootpage FROM __ivm_objects o JOIN sqlite_schema s ON s.name=o.object_name WHERE o.object_type IN('table','index') ORDER BY s.rootpage")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect()
    };
    let before = physical()?;
    db.execute_batch("SELECT sqlite_ivm_rename_source('farms','growers');SELECT sqlite_ivm_rename_column('growers','price','cost');ALTER TABLE earnings RENAME TO income")?;
    assert_eq!(physical()?, before);
    let values = db
        .prepare("SELECT * FROM income ORDER BY region")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        values,
        vec![("north".into(), 25, 30), ("south".into(), 80, 80)]
    );
    Ok(())
}
#[test]
fn null_recursive_keys_weighted_overflow_and_aggregate_exists() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE roots(k INTEGER);CREATE TABLE edges(k INTEGER,v INTEGER);
        CREATE VIRTUAL TABLE reached USING sqlite_ivm('WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.v FROM edges e JOIN r ON e.k=r.n) SELECT n FROM r');
        INSERT INTO roots VALUES(NULL);INSERT INTO edges VALUES(NULL,7);")?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM reached", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        db.query_row("SELECT n FROM reached", [], |r| r.get::<_, Option<i64>>(0))?,
        None
    );
    db.execute_batch("INSERT INTO roots VALUES(1);INSERT INTO edges VALUES(1,2),(2,1);DELETE FROM roots WHERE k=1")?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM reached", [], |r| r.get::<_, i64>(0))?,
        1
    );
    db.execute_batch("CREATE TABLE amounts(v INTEGER);CREATE VIRTUAL TABLE sums USING sqlite_ivm('SELECT SUM(v) AS amount FROM amounts');INSERT INTO amounts VALUES(4611686018427387904)")?;
    assert!(db
        .execute_batch("INSERT INTO amounts VALUES(4611686018427387904)")
        .is_err());
    assert_eq!(
        db.query_row("SELECT amount FROM sums", [], |r| r.get::<_, i64>(0))?,
        4611686018427387904
    );
    assert!(db.execute_batch("CREATE VIRTUAL TABLE bad USING sqlite_ivm('SELECT k FROM roots WHERE EXISTS(SELECT COUNT(*) FROM edges WHERE edges.k=roots.k)')").is_err());
    Ok(())
}
#[test]
fn ignored_and_replaced_source_updates_preserve_arrangements() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE farms(id INTEGER PRIMARY KEY,k INTEGER UNIQUE,v INTEGER);INSERT INTO farms VALUES(1,10,20),(2,11,30),(3,12,40);
        CREATE VIRTUAL TABLE earnings USING sqlite_ivm('SELECT k,MIN(v) AS value FROM farms GROUP BY k')")?;
    let rows = |sql: &str| -> Result<Vec<(i64, i64)>> {
        db.prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    for sql in [
        "UPDATE OR IGNORE farms SET k=10 WHERE id=2",
        "INSERT OR IGNORE INTO farms VALUES(4,10,99)",
        "UPDATE OR IGNORE farms SET k=k-1",
        "UPDATE OR REPLACE farms SET k=10 WHERE id=1",
        "BEGIN;UPDATE OR REPLACE farms SET k=10",
        "ROLLBACK",
    ] {
        db.execute_batch(sql)?;
        assert_eq!(
            rows("SELECT * FROM earnings ORDER BY k")?,
            rows("SELECT k,MIN(v) FROM farms GROUP BY k ORDER BY k")?,
            "{sql}"
        );
    }
    Ok(())
}
