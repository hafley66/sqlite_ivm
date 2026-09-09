#![cfg(not(feature = "extension"))]
use rusqlite::{types::Value, Connection, Result};
#[path = "support/0_database.rs"]
mod database;
use database::register;

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
fn feature_compositions_against_sqlite() -> Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/1_features.json")).unwrap();
    let mut failures = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let query = case["query"].as_str().unwrap();
        let result = (|| -> Result<()> {
            let db = Connection::open_in_memory()?;
            register(&db)?;
            db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
            db.execute_batch(fixture["schema"].as_str().unwrap())?;
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
                query.replace('\'', "''")
            ))?;
            assert_eq!(
                rows(&db, "SELECT * FROM result")?,
                rows(&db, query)?,
                "{name}: empty"
            );
            for (step, sql) in fixture["mutations"].as_array().unwrap().iter().enumerate() {
                let sql = sql.as_str().unwrap();
                db.execute_batch(sql)
                    .unwrap_or_else(|e| panic!("{name}: {step}: {sql}: {e}"));
                assert_eq!(
                    rows(&db, "SELECT * FROM result")?,
                    rows(&db, query)?,
                    "{name}: {step}: {sql}"
                );
            }
            Ok(())
        })();
        if let Err(e) = result {
            failures.push(format!("{name}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn blobs_and_adjacent_floats_survive_trigger_transport() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k BLOB,v REAL);CREATE TABLE b(k BLOB);
        CREATE VIRTUAL TABLE values_view USING sqlite_ivm('SELECT id,k,v,typeof(k) AS kind FROM a');
        CREATE VIRTUAL TABLE joined USING sqlite_ivm('SELECT a.id,a.k FROM a JOIN b ON a.k=b.k');
        CREATE VIRTUAL TABLE distinct_values USING sqlite_ivm('SELECT DISTINCT v FROM a');
        CREATE VIRTUAL TABLE bounds USING sqlite_ivm('SELECT k,MIN(v) AS lo,MAX(v) AS hi FROM a GROUP BY k')")?;
    let queries = [
        ("values_view", "SELECT id,k,v,typeof(k) FROM a"),
        ("joined", "SELECT a.id,a.k FROM a JOIN b ON a.k=b.k"),
        ("distinct_values", "SELECT DISTINCT v FROM a"),
        ("bounds", "SELECT k,MIN(v),MAX(v) FROM a GROUP BY k"),
    ];
    let verify = || -> Result<()> {
        for (view, query) in queries {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM {view}"))?,
                rows(&db, query)?,
                "{view}"
            );
        }
        Ok(())
    };
    for (id, key, value) in [
        (1, vec![], 1.0000000000000002f64),
        (2, vec![0, 255, 0], 1.0000000000000004),
        (3, vec![0, 255, 0], 1.2345678901234567),
        (4, vec![0, 255, 0], 1.2345678901234567),
        (5, vec![], f64::INFINITY),
        (6, vec![], f64::NEG_INFINITY),
        (7, vec![], f64::from_bits(1)),
    ] {
        db.execute(
            "INSERT INTO a VALUES(?1,?2,?3)",
            rusqlite::params![id, key, value],
        )?;
        verify()?;
        db.execute("INSERT INTO b VALUES(?1)", [&key])?;
        verify()?;
    }
    for sql in [
        "BEGIN",
        "UPDATE a SET k=x'00',v=1.0000000000000002 WHERE id=3",
        "DELETE FROM a WHERE id=2",
        "ROLLBACK",
        "DELETE FROM a WHERE id=3",
        "DELETE FROM b WHERE k=x'00ff00'",
        "DELETE FROM a",
    ] {
        db.execute_batch(sql)?;
        verify()?;
    }
    Ok(())
}

#[test]
fn collations_control_group_distinct_join_and_outer_predicates() -> Result<()> {
    for collation in ["NOCASE", "RTRIM"] {
        let db = Connection::open_in_memory()?;
        register(&db)?;
        db.execute_batch(&format!("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k TEXT COLLATE {collation},v INTEGER);CREATE TABLE b(bid INTEGER PRIMARY KEY,k TEXT COLLATE {collation})"))?;
        let normalize = if collation == "NOCASE" {
            "lower"
        } else {
            "rtrim"
        };
        let queries=[format!("SELECT {normalize}(k) AS k,COUNT(*) AS n,SUM(v) AS s,COUNT(DISTINCT k) AS keys FROM a GROUP BY k"),
            format!("SELECT {normalize}(k) AS k FROM (SELECT DISTINCT k FROM a)"),
            "SELECT a.id,b.bid FROM a FULL JOIN b ON a.k=b.k".into(),
            "SELECT a.id,b.bid FROM a FULL JOIN b USING(k)".into(),
            "SELECT id FROM a WHERE k='alpha' COLLATE BINARY".into(),
            "SELECT k FROM a".into()];
        for (i, q) in queries.iter().enumerate() {
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{}')",
                q.replace('\'', "''")
            ))?;
        }
        for sql in [
            "INSERT INTO a VALUES(1,'alpha',7),(2,'ALPHA',9),(3,'alpha ',4),(4,NULL,2)",
            "INSERT INTO b VALUES(1,'alpha'),(2,'ALPHA'),(3,'alpha '),(4,NULL)",
            "UPDATE a SET k='ALPHA' WHERE id=3",
            "DELETE FROM b WHERE bid=1",
            "BEGIN;DELETE FROM a WHERE id=1",
            "ROLLBACK",
            "DELETE FROM a WHERE id=1",
            "UPDATE b SET k='other'",
            "DELETE FROM a",
            "DELETE FROM b",
        ] {
            db.execute_batch(sql)?;
            for (i, q) in queries.iter().enumerate() {
                assert_eq!(
                    rows(&db, &format!("SELECT * FROM v{i}"))?,
                    rows(&db, q)?,
                    "{collation}: {sql}: {q}"
                );
            }
            assert_eq!(
                rows(&db, "SELECT k FROM v5 WHERE k='alpha'")?,
                rows(&db, "SELECT k FROM a WHERE k='alpha'")?
            );
        }
    }
    Ok(())
}

#[test]
fn recursive_delete_preserves_alternate_null_support_and_collated_roots() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE roots(k TEXT COLLATE NOCASE);CREATE TABLE edges(a TEXT COLLATE NOCASE,b TEXT COLLATE NOCASE)")?;
    let query="WITH RECURSIVE r(n) AS(SELECT k FROM roots UNION SELECT e.b FROM edges e JOIN r ON e.a=r.n) SELECT lower(n) AS n FROM r";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE reached USING sqlite_ivm('{query}')"
    ))?;
    for sql in ["INSERT INTO roots VALUES('a'),('A'),('x')","INSERT INTO edges VALUES('A','b'),('B','c'),('c','a'),('B',NULL),('x',NULL),(NULL,'unreachable')","DELETE FROM edges WHERE a='b' AND b IS NULL","DELETE FROM roots WHERE k='a' COLLATE BINARY","DELETE FROM roots WHERE k='A' COLLATE BINARY","BEGIN;DELETE FROM edges WHERE a='x'","ROLLBACK","INSERT INTO roots VALUES(NULL)","DELETE FROM edges WHERE a='x'","DELETE FROM roots","INSERT INTO roots VALUES('A')","DELETE FROM edges WHERE a='b'","DELETE FROM edges","DELETE FROM roots"]{
        db.execute_batch(sql)?;assert_eq!(rows(&db,"SELECT * FROM reached")?,rows(&db,query)?,"{sql}");
    }
    Ok(())
}

#[test]
fn parenthesized_join_scopes_and_nonrecursive_union_cte() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER);CREATE TABLE b(bid INTEGER PRIMARY KEY,k INTEGER);CREATE TABLE c(cid INTEGER PRIMARY KEY,k INTEGER)")?;
    let queries = [
        "SELECT a.id,b.bid,c.cid FROM a LEFT JOIN (b INNER JOIN c ON b.k=c.k) ON a.k=b.k",
        "WITH RECURSIVE pairs(x) AS(SELECT k FROM a UNION SELECT k FROM b) SELECT x FROM pairs",
    ];
    for (i, q) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE v{i} USING sqlite_ivm('{q}')"
        ))?;
    }
    for sql in [
        "INSERT INTO a VALUES(1,1),(2,NULL)",
        "INSERT INTO b VALUES(1,1),(2,2)",
        "INSERT INTO c VALUES(1,1)",
        "UPDATE c SET k=2",
        "DELETE FROM b",
        "DELETE FROM a",
        "DELETE FROM c",
    ] {
        db.execute_batch(sql)?;
        for (i, q) in queries.iter().enumerate() {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM v{i}"))?,
                rows(&db, q)?,
                "{sql}: {q}"
            );
        }
    }
    Ok(())
}

#[test]
fn materialized_output_affinity_matches_ordinary_view_consumers() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;CREATE TABLE a(id INTEGER PRIMARY KEY,label TEXT)")?;
    let query="SELECT id,label,CAST(label AS INTEGER) AS numeric,label COLLATE NOCASE AS folded,(label) AS paren FROM a";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}');CREATE VIEW expected AS {query}"
    ))?;
    for sql in [
        "INSERT INTO a VALUES(1,'1'),(2,'2'),(3,'ALPHA'),(4,NULL)",
        "UPDATE a SET label='01' WHERE id=1",
        "UPDATE a SET label='alpha' WHERE id=3",
        "DELETE FROM a WHERE id=2",
    ] {
        db.execute_batch(sql)?;
        for predicate in [
            "label=1",
            "1=label",
            "label BETWEEN 1 AND 2",
            "label IN(1,2)",
            "id='1'",
            "id=label",
            "numeric='1'",
            "folded=1",
            "paren=1",
            "folded='alpha'",
        ] {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM result WHERE {predicate}"))?,
                rows(&db, &format!("SELECT * FROM expected WHERE {predicate}"))?,
                "{sql}: {predicate}"
            );
        }
    }
    Ok(())
}
