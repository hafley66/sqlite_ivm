// The Group LIMIT path seeds a recursive CTE with each candidate's multiplicity.
// Seeding at min(__n,limit+offset) is only sound because no query reads past that
// many bag rows; a window reads every copy, so the clamp is gated on its absence.
#![cfg(not(feature = "extension"))]
use rusqlite::{types::Value, Connection, Result};
use sqlite_ivm::extension::register;

/// Multiplicity shapes: how many copies each distinct value carries. Chosen so
/// some groups are exhausted by the LIMIT and some are not.
const SHAPES: [&[i64]; 8] = [
    &[1, 1, 1],
    &[3, 1, 1],
    &[1, 3, 1],
    &[5, 5, 5],
    &[7, 2, 1],
    &[1, 1, 9],
    &[4, 4, 1],
    &[11, 1, 2],
];

fn seeded(db: &Connection, shape: &[i64]) -> Result<()> {
    db.execute_batch("DELETE FROM a")?;
    let mut id = 0i64;
    for (value, copies) in shape.iter().enumerate() {
        for _ in 0..*copies {
            id += 1;
            db.execute("INSERT INTO a(id,v) VALUES(?1,?2)", (id, value as i64))?;
        }
    }
    Ok(())
}

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let mut out = statement
        .query_map([], |r| (0..width).map(|i| r.get(i)).collect())?
        .collect::<Result<Vec<Vec<Value>>>>()?;
    out.sort_by_key(|r| format!("{r:?}"));
    Ok(out)
}

#[test]
fn group_limit_matches_plain_sql_across_multiplicity_limit_and_offset() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);",
    )?;
    let mut installed = 0usize;
    for limit in 1..=5i64 {
        for offset in 0..=3i64 {
            let query =
                format!("SELECT v AS value FROM a ORDER BY value LIMIT {limit} OFFSET {offset}");
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE g{installed} USING sqlite_ivm('{query}')"
            ))?;
            for shape in SHAPES {
                seeded(&db, shape)?;
                let view = rows(&db, &format!("SELECT value FROM g{installed}"))?;
                let plain = rows(&db, &query)?;
                assert_eq!(
                    view, plain,
                    "shape {shape:?} limit {limit} offset {offset}"
                );
            }
            installed += 1;
        }
    }
    assert_eq!(installed, 20, "20 limit and offset pairs, 8 shapes each");
    Ok(())
}

#[test]
fn window_with_limit_reads_every_copy() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,v INTEGER);",
    )?;
    // ROW_NUMBER survives a clamp because it only reads as far as the LIMIT. An
    // aggregate over the whole bag does not: dropping a copy changes every row.
    let queries = [
        "SELECT v AS value,ROW_NUMBER() OVER(ORDER BY v,id) AS rank FROM a LIMIT 4",
        "SELECT v AS value,SUM(v) OVER() AS total FROM a LIMIT 4",
        "SELECT v AS value,COUNT(*) OVER() AS seen FROM a LIMIT 4",
    ];
    for (i, query) in queries.iter().enumerate() {
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE w{i} USING sqlite_ivm('{query}')"
        ))?;
        for shape in SHAPES {
            seeded(&db, shape)?;
            let view = rows(&db, &format!("SELECT * FROM w{i}"))?;
            let plain = rows(&db, query)?;
            assert_eq!(view, plain, "shape {shape:?} query {query}");
        }
    }
    Ok(())
}
