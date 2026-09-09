//! Loaded-extension feature acceptance and typed fixtures for independent engines.
use rusqlite::{types::ValueRef, Connection, Result};
use serde_json::{json, Value};
use std::path::Path;
fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut s = db.prepare(sql)?;
    let n = s.column_count();
    let mut result = s
        .query_map([], |r| {
            (0..n)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(n) => json!(n),
                        ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
                        ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidQuery),
                    })
                })
                .collect()
        })?
        .collect::<Result<Vec<Vec<Value>>>>()?;
    result.sort_by_key(|r| serde_json::to_string(r).unwrap());
    Ok(result)
}
fn load(db: &Connection, extension: &str) -> Result<()> {
    unsafe {
        db.load_extension_enable()?;
        db.load_extension(extension, None::<&str>)?;
        db.load_extension_disable()?;
    }
    db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    let extension = &args[2];
    let directory = Path::new(&args[3]);
    std::fs::create_dir_all(directory)?;
    for case in fixture["cases"].as_array().ok_or("cases required")? {
        let name = case["name"].as_str().unwrap();
        let query = case["query"].as_str().unwrap();
        let path = directory.join(format!("{name}.db"));
        let db = Connection::open(&path)?;
        load(&db, extension)?;
        db.execute_batch(fixture["schema"].as_str().unwrap())?;
        let oracle = Connection::open_in_memory()?;
        oracle.execute_batch(fixture["schema"].as_str().unwrap())?;
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE result USING sqlite_ivm('{}')",
            query.replace('\'', "''")
        ))?;
        let columns = db
            .prepare(query)?
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let mut states = vec![];
        for (step, mutation) in std::iter::once("")
            .chain(
                fixture["mutations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap()),
            )
            .enumerate()
        {
            oracle.execute_batch(mutation)?;
            db.execute_batch(mutation)?;
            let actual = rows(&db, "SELECT * FROM result")?;
            let expected = rows(&oracle, query)?;
            if actual != expected {
                return Err(
                    format!("{name} step {step}: {mutation}: {actual:?} != {expected:?}").into(),
                );
            }
            for table in ["a", "b", "c"] {
                let sql = format!("SELECT * FROM {table}");
                if rows(&db, &sql)? != rows(&oracle, &sql)? {
                    return Err(format!("{name} step {step}: source {table} mismatch").into());
                }
            }
            states.push(json!({"step":step,"mutation":mutation,"expected":expected,"inputs":{"a":rows(&oracle,"SELECT * FROM a ORDER BY id")?,"b":rows(&oracle,"SELECT * FROM b ORDER BY bid")?,"c":rows(&oracle,"SELECT * FROM c ORDER BY cid")?}}));
        }
        let final_rows = rows(&db, query)?;
        drop(db);
        let db = Connection::open(&path)?;
        load(&db, extension)?;
        if rows(&db, "SELECT * FROM result")? != final_rows {
            return Err(format!("{name}: reopen mismatch").into());
        }
        db.execute_batch("BEGIN;DELETE FROM a;DELETE FROM b;DELETE FROM c")?;
        if rows(&db, "SELECT * FROM result")? != rows(&db, query)? {
            return Err(format!("{name}: reopened mutation mismatch").into());
        }
        db.execute_batch("ROLLBACK")?;
        if rows(&db, "SELECT * FROM result")? != final_rows {
            return Err(format!("{name}: reopened rollback mismatch").into());
        }
        let artifact =
            json!({"case":case,"schema":fixture["schema"],"columns":columns,"states":states});
        std::fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec(&artifact)?,
        )?;
        println!(
            "{}",
            json!({"engine":"sqlite-ivm-native","case":name,"states":states.len(),"status":"ok","reopen":true,"reopened_mutation_rollback":true,"sqlite_version":rusqlite::version()})
        );
    }
    Ok(())
}
