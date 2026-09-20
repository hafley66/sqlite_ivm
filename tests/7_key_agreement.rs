#![cfg(not(feature = "extension"))]
//! Encoder ownership:
//! - `key()` after `key_expression` feeds Set, Join, Group, and fixpoint input
//!   maintenance in `src/1a_relational.rs:35`.
//! - `identity()` stores arrangement row identities and result state keys in
//!   `src/1a_relational.rs:52`.
//! - `key_sql()` feeds recursive `all` and `work` tables in
//!   `src/0b_relational.rs:177`.
//!
//! What the encoders produce is now interned, so `__k` holds a dictionary id
//! and the composite this file compares comes back out of the dictionary.
//!
//! JSON-subtype group keys are confirmed as divergent. Expression group keys
//! are admitted by `src/0b_relational.rs:1339`; bulk `fill` preserves the JSON
//! subtype while incremental Group maintenance receives a plain text value at
//! `src/1a_relational.rs:1102`. The final test records the resulting split.

use rusqlite::{types::Value, Connection, Result};
use sqlite_ivm::relational::{key_expression, key_sql};

#[path = "support/0_database.rs"]
mod database;
use database::register;

const CORPUS_SIZE: usize = 17;
const COLLATION_COUNT: usize = 3;
const COLLATIONS: [&str; COLLATION_COUNT] = ["BINARY", "NOCASE", "RTRIM"];

struct Case {
    name: &'static str,
    value: Value,
}

fn corpus() -> Vec<Case> {
    let cases = vec![
        Case {
            name: "integer zero",
            value: Value::Integer(0),
        },
        Case {
            name: "integer one",
            value: Value::Integer(1),
        },
        Case {
            name: "integral real one",
            value: Value::Real(1.0),
        },
        Case {
            name: "negative integer",
            value: Value::Integer(-1),
        },
        Case {
            name: "non integral real",
            value: Value::Real(1.5),
        },
        Case {
            name: "same non integral real",
            value: Value::Real(1.5),
        },
        Case {
            name: "negative zero",
            value: Value::Real(-0.0),
        },
        Case {
            name: "NaN",
            value: Value::Real(f64::NAN),
        },
        Case {
            name: "positive infinity",
            value: Value::Real(f64::INFINITY),
        },
        Case {
            name: "negative infinity",
            value: Value::Real(f64::NEG_INFINITY),
        },
        Case {
            name: "i64 maximum integer",
            value: Value::Integer(i64::MAX),
        },
        Case {
            name: "i64 maximum real boundary",
            value: Value::Real(i64::MAX as f64),
        },
        Case {
            name: "integer beyond f64 exact range",
            value: Value::Integer(9_007_199_254_740_993),
        },
        Case {
            name: "empty blob",
            value: Value::Blob(vec![]),
        },
        Case {
            name: "null byte blob",
            value: Value::Blob(vec![0]),
        },
        Case {
            name: "tagged blob text",
            value: Value::Text(r#"{"blob":"00"}"#.into()),
        },
        Case {
            name: "NULL",
            value: Value::Null,
        },
    ];
    assert_eq!(
        cases.len(),
        CORPUS_SIZE,
        "CORPUS_SIZE protects the bounded value matrix"
    );
    cases
}

fn rendered_rows(db: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let mut rows = statement
        .query_map([], |row| {
            Ok((0..width)
                .map(|index| row.get::<_, Value>(index).map(|value| format!("{value:?}")))
                .collect::<Result<Vec<_>>>()?
                .join(" | "))
        })?
        .collect::<Result<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

fn arrangement_name(db: &Connection, view: &str) -> Result<String> {
    db.query_row(
        "SELECT object_name FROM __ivm_objects WHERE view_name=?1 AND object_type='table' AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__k') AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__r') LIMIT 1",
        [view],
        |row| row.get(0),
    )
}

fn create_view(db: &Connection, view: &str, query: &str) -> Result<()> {
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {view} USING sqlite_ivm('{}')",
        query.replace('\'', "''")
    ))
}

fn sql_keys(db: &Connection, source: &str, collation: &str) -> Result<Vec<String>> {
    let normalized = key_expression("value", collation);
    let expression = key_sql(&[(normalized, "BINARY".into())]);
    let mut statement = db.prepare(&format!("SELECT {expression} FROM {source} ORDER BY id"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<String>>>()?;
    Ok(rows)
}

fn dictionary_name(db: &Connection, view: &str) -> Result<String> {
    db.query_row(
        "SELECT object_name FROM __ivm_objects WHERE view_name=?1 AND object_type='table' AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__v') LIMIT 1",
        [view],
        |row| row.get(0),
    )
}

/// `__k` is a dictionary id on interned operator kinds, so the composite the
/// oracle compares against comes back through the dictionary, not the column.
fn rust_keys(db: &Connection, view: &str, arrangement: &str) -> Result<Vec<String>> {
    let dictionary = dictionary_name(db, view)?;
    let mut statement = db.prepare(&format!(
        "SELECT d.__v FROM {arrangement} a JOIN \"{dictionary}\" d ON d.__i=a.__k ORDER BY a.c0"
    ))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<String>>>()?;
    Ok(rows)
}

#[test]
fn rust_key_after_expression_agrees_with_sql_key_sql() -> Result<()> {
    let cases = corpus();
    assert_eq!(COLLATIONS.len(), COLLATION_COUNT);
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;

    for (collation_index, collation) in COLLATIONS.iter().enumerate() {
        assert!(
            collation_index < COLLATION_COUNT,
            "COLLATION_COUNT protects the bounded collation matrix"
        );
        let source = format!("key_source_{collation_index}");
        let incremental = format!("key_incremental_{collation_index}");
        let bulk = format!("key_bulk_{collation_index}");
        db.execute_batch(&format!(
            "CREATE TABLE {source}(id INTEGER PRIMARY KEY,value COLLATE {collation})"
        ))?;
        let query = format!("SELECT value,COUNT(*) AS n FROM {source} GROUP BY value");
        create_view(&db, &incremental, &query)?;

        for (index, case) in cases.iter().enumerate() {
            assert!(
                index < CORPUS_SIZE,
                "CORPUS_SIZE protects the bounded incremental insert loop"
            );
            db.execute(
                &format!("INSERT INTO {source}(id,value) VALUES(?1,?2)"),
                rusqlite::params![index as i64, case.value.clone()],
            )?;
        }

        create_view(&db, &bulk, &query)?;
        let arrangement = arrangement_name(&db, &incremental)?;
        let rust = rust_keys(&db, &incremental, &arrangement)?;
        let sql = sql_keys(&db, &source, collation)?;
        assert_eq!(rust.len(), CORPUS_SIZE, "{collation}: arrangement rows");
        assert_eq!(sql.len(), CORPUS_SIZE, "{collation}: oracle rows");

        for (left, rust_left) in rust.iter().enumerate() {
            assert!(
                left < CORPUS_SIZE,
                "CORPUS_SIZE protects the bounded Rust key comparison loop"
            );
            for (right, rust_right) in rust.iter().enumerate() {
                assert!(
                    right < CORPUS_SIZE,
                    "CORPUS_SIZE protects the bounded Rust key comparison loop"
                );
                assert_eq!(
                    rust_left == rust_right,
                    sql[left] == sql[right],
                    "{collation}: {} and {}: Rust and SQL key partitions differ",
                    cases[left].name,
                    cases[right].name
                );
            }
        }

        assert_eq!(
            rendered_rows(&db, &format!("SELECT * FROM {incremental}"))?,
            rendered_rows(&db, &format!("SELECT * FROM {bulk}"))?,
            "{collation}: incremental and bulk groups"
        );
    }
    Ok(())
}

#[test]
fn identity_round_trips_the_corpus() -> Result<()> {
    let cases = corpus();
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE identity_source(id INTEGER PRIMARY KEY,value);
         CREATE VIRTUAL TABLE identity_view USING sqlite_ivm('SELECT value FROM identity_source')",
    )?;

    for (index, case) in cases.iter().enumerate() {
        assert!(
            index < CORPUS_SIZE,
            "CORPUS_SIZE protects the bounded identity insert loop"
        );
        db.execute(
            "INSERT INTO identity_source(id,value) VALUES(?1,?2)",
            rusqlite::params![index as i64, case.value.clone()],
        )?;
        assert_eq!(
            rendered_rows(&db, "SELECT value FROM identity_view")?,
            rendered_rows(&db, "SELECT value FROM identity_source")?,
            "identity after {}",
            case.name
        );
    }

    for (index, case) in cases.iter().enumerate().rev() {
        assert!(
            index < CORPUS_SIZE,
            "CORPUS_SIZE protects the bounded identity delete loop"
        );
        db.execute("DELETE FROM identity_source WHERE id=?1", [index as i64])?;
        assert_eq!(
            rendered_rows(&db, "SELECT value FROM identity_view")?,
            rendered_rows(&db, "SELECT value FROM identity_source")?,
            "identity after deleting {}",
            case.name
        );
    }
    Ok(())
}

#[test]
fn json_subtype_group_key_diverges_between_bulk_and_incremental_paths() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE json_source(id INTEGER PRIMARY KEY,j TEXT);
         INSERT INTO json_source VALUES(1,'{\"t\":[\"a\"]}')",
    )?;
    let query = "SELECT json_extract(j,'$.t') AS t,COUNT(*) AS n FROM json_source GROUP BY json_extract(j,'$.t')";
    create_view(&db, "json_bulk", query)?;
    assert_eq!(
        rendered_rows(&db, "SELECT * FROM json_bulk")?,
        vec![r#"Text("[\"a\"]") | Integer(1)"#.to_string()]
    );

    db.execute(
        "INSERT INTO json_source VALUES(?1,?2)",
        [Value::Integer(2), Value::Text(r#"{"t":["a"]}"#.into())],
    )?;
    assert_eq!(
        rendered_rows(&db, query)?,
        vec![r#"Text("[\"a\"]") | Integer(2)"#.to_string()]
    );
    assert_eq!(
        rendered_rows(&db, "SELECT * FROM json_bulk")?,
        vec![
            r#"Text("[\"a\"]") | Integer(1)"#.to_string(),
            r#"Text("[\"a\"]") | Integer(1)"#.to_string(),
        ]
    );
    Ok(())
}
