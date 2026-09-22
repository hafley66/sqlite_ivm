#![cfg(not(feature = "extension"))]
//! The dictionary is the only place a composite key lives as text. Interning
//! must be injective and idempotent, or two equal keys reach two arrangements.

use rusqlite::{types::Value, Connection, OptionalExtension, Result};
use sqlite_ivm::relational::{key_expression, key_sql};
use sqlite_ivm::relational_maintenance::{identity_sql, intern, resolve, row_hash};

#[path = "support/0_database.rs"]
mod database;
use database::register;

#[test]
fn source_reads_survive_hash_collisions_and_migrate_copied_inputs() -> Result<()> {
    for legacy in [false, true] {
        let path = std::env::temp_dir().join(format!("ivm-row-identity-{}-{legacy}.db", std::process::id()));
        assert!(!path.exists(), "test receipt already exists: {}", path.display());
        if legacy { std::fs::write(&path, include_bytes!("fixtures/4_copied_inputs_v6.db")).unwrap(); }
        if !legacy {
            let db = Connection::open(&path)?;
            register(&db)?;
            db.create_scalar_function(c"sqlite_ivm_hash", 1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
                |_| Ok(0i64))?;
            db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
                CREATE TABLE a(k INTEGER,v INTEGER);
                CREATE VIRTUAL TABLE g USING sqlite_ivm('SELECT k,count(*) AS n,sum(v) AS s FROM a GROUP BY k');
                INSERT INTO a VALUES(1,2),(1,2),(1,3),(2,9)")?;

        }
        {
            let db = Connection::open(&path)?;
            register(&db)?;
            db.create_scalar_function(c"sqlite_ivm_hash", 1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
                |_| Ok(0i64))?;
            db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
            assert_eq!(db.query_row("SELECT count(*) FROM g", [], |r| r.get::<_,i64>(0))?, 2);
            assert_eq!(db.query_row("SELECT format_version FROM __ivm_schema", [], |r| r.get::<_,i64>(0))?, 8);
            assert!(arrangements(&db,"g")?.is_empty());
            for mutation in [
                "BEGIN;UPDATE a SET v=v+1 WHERE k=1;INSERT INTO a VALUES(2,10);COMMIT",
                "BEGIN;DELETE FROM a WHERE v=3;UPDATE a SET k=3 WHERE k=2;ROLLBACK",
                "DELETE FROM a WHERE v=3",
                "DELETE FROM a",
            ] {
                db.execute_batch(mutation)?;
                let read = |sql| -> Result<Vec<(i64,i64,Option<i64>)>> {
                    db.prepare(sql)?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect()
                };
                assert_eq!(read("SELECT * FROM g ORDER BY k")?, read("SELECT k,count(*),sum(v) FROM a GROUP BY k ORDER BY k")?, "legacy={legacy}: {mutation}");
                let check: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                assert_eq!(check, "ok");
            }
        }
        std::fs::remove_file(path).unwrap();
    }
    Ok(())
}

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
fn assert_result_identity_without_copied_inputs(db: &Connection, view: &str, at: &str) -> Result<()> {
    assert!(arrangements(db,view)?.is_empty(), "{view}: persistent input rows at {at}");
    let width: i64 = db.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name NOT LIKE '__ivm_%'",[view],|r|r.get(0))?;
    let composite = format!("sqlite_ivm_row_check({})",(0..width).map(|i|format!("c{i}")).collect::<Vec<_>>().join(","));
    let invalid: i64 = db.query_row(&format!("SELECT count(*) FROM \"{view}_state\" WHERE __check!={composite}"),[],|r|r.get(0))?;
    assert_eq!(invalid,0,"{view}: result identity at {at}");
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
        assert_result_identity_without_copied_inputs(&db, "hash_view", name)?;
        db.execute(
            "INSERT INTO hash_source(id,value) VALUES(?1,?2)",
            rusqlite::params![(index + CORPUS_SIZE) as i64, value],
        )?;
        assert_result_identity_without_copied_inputs(&db, "hash_view", name)?;
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
    let composite = identity_sql(1);
    let distinct: i64 = db.query_row(&format!("SELECT count(DISTINCT {composite}) FROM (SELECT value AS c0 FROM folded_source)"),[],|r|r.get(0))?;
    assert_eq!(distinct,3,"integer, real, and NULL retain distinct identities");
    let result_count: i64 = db.query_row("SELECT count(*) FROM folded_view",[],|r|r.get(0))?;
    assert_eq!(result_count,2,"integer and real share SQL equality; duplicate NULLs produce one result");
    db.execute_batch("DELETE FROM folded_source WHERE id IN (1,3)")?;
    assert_eq!(db.query_row("SELECT count(*) FROM folded_view",[],|r|r.get::<_,i64>(0))?,2);
    db.execute_batch("DELETE FROM folded_source")?;
    assert_eq!(db.query_row("SELECT count(*) FROM folded_view",[],|r|r.get::<_,i64>(0))?,0);
    assert_result_identity_without_copied_inputs(&db, "folded_view", "folded corpus")?;
    Ok(())
}

#[test]
fn the_row_hash_is_the_published_fnv1a_64_vector() -> Result<()> {
    assert_eq!(row_hash(b""), 0xcbf2_9ce4_8422_2325_u64 as i64);
    assert_eq!(row_hash(b"a"), 0xaf63_dc4c_8601_ec8c_u64 as i64);
    assert_eq!(row_hash(b"foobar"), 0x85944171f73967e8_u64 as i64);
    Ok(())
}
