#![cfg(not(feature = "extension"))]
//! The dictionary is the only place a composite key lives as text. Interning
//! must be injective and idempotent, or two equal keys reach two arrangements.

use rusqlite::{types::Value, Connection, OptionalExtension, Result};
use sqlite_ivm::relational::{key_expression, key_sql};
use sqlite_ivm::relational_maintenance::{intern, resolve};

#[path = "support/0_database.rs"]
mod database;
use database::register;

/// Mirrors the corpus in `tests/7_key_agreement.rs`, which owns the encoder
/// agreement; this file owns only what the dictionary does with the result.
const CORPUS_SIZE: usize = 17;
const COLLATION_COUNT: usize = 3;
const COLLATIONS: [&str; COLLATION_COUNT] = ["BINARY", "NOCASE", "RTRIM"];

fn corpus() -> Vec<(&'static str, Value)> {
    let cases = vec![
        ("integer zero", Value::Integer(0)),
        ("integer one", Value::Integer(1)),
        ("integral real one", Value::Real(1.0)),
        ("negative integer", Value::Integer(-1)),
        ("non integral real", Value::Real(1.5)),
        ("same non integral real", Value::Real(1.5)),
        ("negative zero", Value::Real(-0.0)),
        ("NaN", Value::Real(f64::NAN)),
        ("positive infinity", Value::Real(f64::INFINITY)),
        ("negative infinity", Value::Real(f64::NEG_INFINITY)),
        ("i64 maximum integer", Value::Integer(i64::MAX)),
        ("i64 maximum real boundary", Value::Real(i64::MAX as f64)),
        (
            "integer beyond f64 exact range",
            Value::Integer(9_007_199_254_740_993),
        ),
        ("empty blob", Value::Blob(vec![])),
        ("null byte blob", Value::Blob(vec![0])),
        ("tagged blob text", Value::Text(r#"{"blob":"00"}"#.into())),
        ("NULL", Value::Null),
    ];
    assert_eq!(
        cases.len(),
        CORPUS_SIZE,
        "CORPUS_SIZE protects the bounded value matrix"
    );
    cases
}

fn view_database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE intern_source(id INTEGER PRIMARY KEY,value);
         CREATE VIRTUAL TABLE intern_view USING sqlite_ivm(
           'SELECT value, COUNT(*) AS n FROM intern_source GROUP BY value')",
    )?;
    Ok(db)
}

fn dictionary(db: &Connection, view: &str) -> Result<String> {
    db.query_row(
        "SELECT object_name FROM __ivm_objects WHERE view_name=?1 AND object_type='table' AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__i') AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__v') LIMIT 1",
        [view],
        |row| row.get::<_, String>(0),
    )
    .map(|name| format!("main.\"{name}\""))
}

/// The composites the engine actually interns, produced by the SQL encoder so
/// this file never grows a second copy of the encoding rules.
fn composites(db: &Connection) -> Result<Vec<String>> {
    let cases = corpus();
    let mut values = vec![];
    for (collation_index, collation) in COLLATIONS.iter().enumerate() {
        assert!(
            collation_index < COLLATION_COUNT,
            "COLLATION_COUNT protects the bounded collation matrix"
        );
        let expression = key_sql(&[(key_expression("?1", collation), "BINARY".into())]);
        for (index, (_, value)) in cases.iter().enumerate() {
            assert!(
                index < CORPUS_SIZE,
                "CORPUS_SIZE protects the bounded composite loop"
            );
            values.push(db.query_row(&format!("SELECT {expression}"), [value], |row| {
                row.get::<_, String>(0)
            })?);
        }
    }
    Ok(values)
}

fn rows_in(db: &Connection, table: &str) -> Result<i64> {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))
}

#[test]
fn interning_is_injective_and_resolve_round_trips_the_corpus() -> Result<()> {
    let db = view_database()?;
    let dict = dictionary(&db, "intern_view")?;
    let values = composites(&db)?;
    assert_eq!(values.len(), CORPUS_SIZE * COLLATION_COUNT);

    let ids = values
        .iter()
        .map(|value| intern(&db, &dict, value))
        .collect::<Result<Vec<i64>>>()?;

    for (left, left_value) in values.iter().enumerate() {
        assert!(
            left < CORPUS_SIZE * COLLATION_COUNT,
            "the bounded corpus matrix protects the comparison loop"
        );
        assert_eq!(
            resolve(&db, &dict, ids[left])?,
            *left_value,
            "resolve round trip at {left}"
        );
        for (right, right_value) in values.iter().enumerate() {
            assert_eq!(
                ids[left] == ids[right],
                left_value == right_value,
                "id equality must match composite equality at {left} and {right}"
            );
        }
    }
    Ok(())
}

#[test]
fn re_interning_adds_no_row_and_moves_no_id() -> Result<()> {
    let db = view_database()?;
    let dict = dictionary(&db, "intern_view")?;
    let values = composites(&db)?;

    let first = values
        .iter()
        .map(|value| intern(&db, &dict, value))
        .collect::<Result<Vec<i64>>>()?;
    let after_first = rows_in(&db, &dict)?;
    let second = values
        .iter()
        .map(|value| intern(&db, &dict, value))
        .collect::<Result<Vec<i64>>>()?;

    assert_eq!(first, second, "interning is idempotent");
    assert_eq!(
        rows_in(&db, &dict)?,
        after_first,
        "re-interning writes no dictionary row"
    );
    Ok(())
}

#[test]
fn dropping_the_view_drops_its_dictionary() -> Result<()> {
    let db = view_database()?;
    let dict = dictionary(&db, "intern_view")?;
    intern(&db, &dict, "[]")?;
    db.execute_batch("DROP TABLE intern_view")?;
    let survivor: Option<String> = db
        .query_row(
            "SELECT name FROM sqlite_schema WHERE name='intern_view_keys'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    assert_eq!(survivor, None, "the dictionary is view-scoped storage");
    Ok(())
}
