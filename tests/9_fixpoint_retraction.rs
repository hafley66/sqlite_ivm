#![cfg(not(feature = "extension"))]

use rusqlite::{types::Value, Connection, Result};
use sqlite_ivm::extension::register;

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let mut rows = statement
        .query_map([], |row| (0..width).map(|index| row.get(index)).collect())?
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by_key(|row| format!("{row:?}"));
    Ok(rows)
}

fn normalized_rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut rows = rows(db, sql)?
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|value| match value {
                    Value::Text(value) => Value::Text(value.to_lowercase()),
                    Value::Real(value) if value.fract() == 0.0 => Value::Integer(value as i64),
                    value => value,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| format!("{row:?}"));
    rows.dedup();
    Ok(rows)
}

fn assert_oracle(db: &Connection, recursive: &str) -> Result<()> {
    // Recursive and DISTINCT outputs match up to __k equality, with
    // representatives normalized before comparison.
    assert_eq!(
        normalized_rows(db, "SELECT x,y FROM reached")?,
        normalized_rows(db, recursive)?
    );
    assert_eq!(
        normalized_rows(db, "SELECT x FROM distinct_reached")?,
        normalized_rows(db, &format!("SELECT DISTINCT x FROM ({recursive})"))?
    );
    Ok(())
}

fn run_case(schema: &str, seed: &str, first_delete: &str, second_delete: &str) -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(&format!(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE t(a {schema},b {schema});\
         {seed}"
    ))?;
    let recursive = "WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p";
    let distinct = "WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT DISTINCT x FROM p";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE reached USING sqlite_ivm('{recursive}');\
         CREATE VIRTUAL TABLE distinct_reached USING sqlite_ivm('{distinct}')"
    ))?;
    assert_oracle(&db, recursive).expect("initial oracle");
    db.execute_batch(first_delete).expect("first delete");
    assert_oracle(&db, recursive).expect("oracle after first delete");
    db.execute_batch(second_delete).expect("second delete");
    assert_oracle(&db, recursive).expect("oracle after second delete");
    Ok(())
}

#[test]
fn fixpoint_retraction_emits_the_stored_representative() -> Result<()> {
    run_case(
        "TEXT COLLATE NOCASE",
        "INSERT INTO t VALUES('a','b'),('c','B'),('A','c');",
        "DELETE FROM t WHERE a='a' COLLATE BINARY AND b='b' COLLATE BINARY",
        "DELETE FROM t WHERE a='A' COLLATE BINARY AND b='c' COLLATE BINARY",
    )
    .expect("NOCASE case");
    run_case(
        "",
        "INSERT INTO t VALUES(1,2),(3,2.0),(1.0,3);",
        "DELETE FROM t WHERE typeof(a)='integer' AND a=1 AND typeof(b)='integer' AND b=2",
        "DELETE FROM t WHERE typeof(a)='real' AND a=1 AND typeof(b)='integer' AND b=3",
    )
    .expect("integral real case");
    Ok(())
}
