#![cfg(not(feature = "extension"))]
//! The dictionary is the only place a composite key lives as text. Interning
//! must be injective and idempotent, or two equal keys reach two arrangements.

use rusqlite::{types::Value, Connection, OptionalExtension, Result};
use sqlite_ivm::relational::{key_expression, key_sql};
use sqlite_ivm::relational_maintenance::{identity_sql, intern, resolve, row_hash};

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

#[test]
fn write_path_keys_are_dictionary_ids() -> Result<()> {
    let db = view_database()?;
    db.execute_batch(
        "INSERT INTO intern_source VALUES(1,'north'),(2,'north'),(3,'south')",
    )?;
    let state_type: String = db.query_row(
        "SELECT type FROM pragma_table_info('intern_view_state') WHERE name='__key'",
        [],
        |row| row.get(0),
    )?;
    let missing: i64 = db.query_row(
        "SELECT count(*) FROM intern_view_state s LEFT JOIN intern_view_keys k ON k.__i=s.__key WHERE k.__i IS NULL",
        [],
        |row| row.get(0),
    )?;
    let delta: String = db.query_row(
        "SELECT name FROM sqlite_temp_schema WHERE name LIKE '__ivm_delta_%' LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let delta_type: String = db.query_row(
        "SELECT type FROM pragma_table_info(?1) WHERE name='__v'",
        [&delta],
        |row| row.get(0),
    )?;
    db.execute_batch(
        "CREATE TABLE edges(a INTEGER,b INTEGER);\
         CREATE VIRTUAL TABLE reach USING sqlite_ivm(\
           'WITH RECURSIVE p(x,y) AS (\
              SELECT a,b FROM edges UNION SELECT p.x,e.b FROM p JOIN edges e ON e.a=p.y\
            ) SELECT x,y FROM p')",
    )?;
    let deleted: String = db.query_row(
        "SELECT name FROM sqlite_temp_schema WHERE name LIKE '__ivm_deleted_%' LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let deleted_type: String = db.query_row(
        "SELECT type FROM pragma_table_info(?1) WHERE name='__k'",
        [&deleted],
        |row| row.get(0),
    )?;
    assert_eq!(
        (state_type, missing, delta_type, deleted_type),
        (
            "INTEGER".to_string(),
            0,
            "INTEGER".to_string(),
            "INTEGER".to_string()
        )
    );
    Ok(())
}

/// Every arrangement table for a view, with the width of its `c{i}` columns.
fn arrangements(db: &Connection, view: &str) -> Result<Vec<(String, usize)>> {
    let names = db
        .prepare("SELECT object_name FROM __ivm_objects WHERE view_name=?1 AND object_type='table' AND EXISTS (SELECT 1 FROM pragma_table_info(object_name) WHERE name='__r')")?
        .query_map([view], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    names
        .into_iter()
        .map(|name| {
            let width: i64 = db.query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE name LIKE 'c%'",
                [&name],
                |row| row.get(0),
            )?;
            Ok((name, width as usize))
        })
        .collect()
}

/// `__r` stopped being UNIQUE when it became a hash, so one row per composite
/// is an invariant of the maintenance code and needs its own rail.
fn assert_one_row_per_composite(db: &Connection, view: &str, at: &str) -> Result<()> {
    for (table, width) in arrangements(db, view)? {
        let composite = identity_sql(width);
        let (rows, distinct, stored): (i64, i64, i64) = db.query_row(
            &format!("SELECT count(*),count(DISTINCT {composite}),count(*) FILTER (WHERE sqlite_ivm_hash({composite})=__r) FROM \"{table}\""),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(rows, distinct, "{at}: {table} holds a duplicate composite");
        assert_eq!(
            rows, stored,
            "{at}: {table} stored a hash its own columns do not rebuild"
        );
    }
    Ok(())
}

#[test]
fn hashing_the_identity_keeps_one_arrangement_row_per_composite() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE hash_source(id INTEGER PRIMARY KEY,value);
         CREATE VIRTUAL TABLE hash_view USING sqlite_ivm(
           'SELECT DISTINCT value FROM hash_source')",
    )?;
    for (index, (name, value)) in corpus().into_iter().enumerate() {
        assert!(
            index < CORPUS_SIZE,
            "CORPUS_SIZE protects the bounded insert loop"
        );
        db.execute(
            "INSERT INTO hash_source(id,value) VALUES(?1,?2)",
            rusqlite::params![index as i64, value.clone()],
        )?;
        assert_one_row_per_composite(&db, "hash_view", name)?;
        db.execute(
            "INSERT INTO hash_source(id,value) VALUES(?1,?2)",
            rusqlite::params![(index + CORPUS_SIZE) as i64, value],
        )?;
        assert_one_row_per_composite(&db, "hash_view", name)?;
    }
    Ok(())
}

/// The group key folds integer 1 into real 1.0 on purpose, and SQLite's own
/// UNIQUE would call two NULLs distinct. The identity behind `__r` does
/// neither, so one `__k` carries two rows and two NULLs carry one.
#[test]
fn the_identity_splits_where_the_key_folds_and_joins_where_unique_would_split() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE folded_source(id INTEGER PRIMARY KEY,value);
         CREATE VIRTUAL TABLE folded_view USING sqlite_ivm(
           'SELECT DISTINCT value FROM folded_source');
         INSERT INTO folded_source(id,value) VALUES(1,1),(2,1.0),(3,NULL),(4,NULL)",
    )?;
    let (table, width) = arrangements(&db, "folded_view")?.remove(0);
    let composite = identity_sql(width);

    let folded: i64 = db.query_row(
        &format!("SELECT count(*) FROM \"{table}\" WHERE __k=(SELECT __k FROM \"{table}\" WHERE typeof(c0)='integer')"),
        [],
        |row| row.get(0),
    )?;
    assert_eq!(folded, 2, "integer 1 and real 1.0 share one __k");

    let nulls: (i64, i64) = db.query_row(
        &format!("SELECT count(*),coalesce(sum(__n),0) FROM \"{table}\" WHERE c0 IS NULL"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(nulls, (1, 2), "two NULL rows are one identity at multiplicity 2");

    let distinct: i64 = db.query_row(
        &format!("SELECT count(DISTINCT {composite}) FROM \"{table}\""),
        [],
        |row| row.get(0),
    )?;
    assert_eq!(distinct, 3, "integer 1, real 1.0 and NULL are three identities");
    assert_one_row_per_composite(&db, "folded_view", "folded corpus")?;
    Ok(())
}

#[test]
fn the_row_hash_is_the_published_fnv1a_64_vector() -> Result<()> {
    assert_eq!(row_hash(b""), 0xcbf2_9ce4_8422_2325_u64 as i64);
    assert_eq!(row_hash(b"a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
    assert_eq!(row_hash(b"foobar"), 0x85944171f73967e8_u64 as i64);
    Ok(())
}
