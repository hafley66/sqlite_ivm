//! Native SQLite consumer for the existing shared fixture/receipt protocol.
use rusqlite::{Connection, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};
fn argument(name: &str) -> Option<String> {
    let args = std::env::args().collect::<Vec<_>>();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn output(rows: &[Vec<i64>], prefix: &str) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{prefix}\t{}\n",
                r.iter().map(i64::to_string).collect::<Vec<_>>().join("\t")
            )
        })
        .collect()
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<i64>>> {
    let mut s = db.prepare(sql)?;
    let n = s.column_count();
    let mut rows = s
        .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
        .collect::<Result<Vec<Vec<i64>>>>()?;
    rows.sort();
    Ok(rows)
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(
        argument("--fixture").ok_or("--fixture required")?,
    )?)?;
    let path = PathBuf::from(argument("--db").ok_or("--db required")?);
    let db = Connection::open(&path)?;
    let start = Instant::now();
    db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);CREATE INDEX a_k ON a(k);CREATE INDEX a_v ON a(v);
        CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);CREATE INDEX b_k ON b(k);CREATE INDEX b_v ON b(v);
        CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);CREATE INDEX c_k ON c(k);CREATE INDEX c_v ON c(v);")?;
    let query = fixture["query"].as_str().ok_or("query required")?;
    let extension = argument("--extension");
    if let Some(path) = &extension {
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(path, None::<&str>)?;
            db.load_extension_disable()?;
        }
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE circuit_view USING sqlite_ivm('{}')",
            query.replace('\'', "''")
        ))?;
    }
    let materialize = if extension.is_some() {
        "SELECT * FROM circuit_view"
    } else {
        query
    };
    println!(
        "{}",
        json!({"event":"case-setup","status":"ok","setup_ms":start.elapsed().as_secs_f64()*1000.,"sqlite_version":rusqlite::version(),"algorithm":if extension.is_some(){"sqlite-ivm-persistent-relational"}else{"full-query"},"durability":"WAL synchronous FULL"})
    );
    let (mut total, mut input_hash, mut checksum) = (0., String::new(), String::new());
    for state in fixture["states"].as_array().ok_or("states required")? {
        let start = Instant::now();
        db.execute_batch(&format!(
            "BEGIN;{} COMMIT;",
            state["mutation_sql"]
                .as_str()
                .ok_or("mutation SQL required")?
        ))?;
        let update = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let actual = rows(&db, materialize)?;
        let compute = start.elapsed().as_secs_f64() * 1000.;
        let expected: Vec<Vec<i64>> = serde_json::from_value(state["expected"]["rows"].clone())?;
        if actual != expected {
            return Err(
                format!("{} {}: output mismatch", fixture["circuit"], state["name"]).into(),
            );
        }
        if actual != rows(&db, query)? {
            return Err("SQL oracle mismatch".into());
        }
        let mut inputs = String::new();
        for table in ["a", "b", "c"] {
            let actual = rows(&db, &format!("SELECT id,k,v FROM {table}"))?;
            let mut expected: Vec<Vec<i64>> =
                serde_json::from_value(state["inputs"][table].clone())?;
            expected.sort();
            if actual != expected {
                return Err("source rows mismatch".into());
            }
            inputs.push_str(&output(&actual, &table.to_ascii_uppercase()));
        }
        input_hash = hash(&inputs);
        let canonical = output(&actual, "S");
        checksum = hash(&canonical);
        if input_hash != state["input_hash"] || checksum != state["expected"]["checksum"] {
            return Err("checksum mismatch".into());
        }
        total += update + compute;
        println!(
            "{}",
            json!({"event":"mutation","status":"ok","state":state["name"],"exact_input_output_validated":true,"input_hash":input_hash,"checksum":checksum,"affected_rows":state["writes"].as_array().unwrap().len(),"output_rows":actual.len(),"output_bytes":canonical.len(),"update_transaction_ms":update,"query_compute_ms":compute,"update_plus_query_ms":update+compute})
        );
    }
    if let Some(extension) = &extension {
        // Persistence and DDL checks are outside the shared timing interval.
        let expected = rows(&db, materialize)?;
        drop(db);
        let db = Connection::open(&path)?;
        unsafe {
            db.load_extension_enable()?;
            db.load_extension(extension, None::<&str>)?;
            db.load_extension_disable()?;
        }
        db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;")?;
        if rows(&db, materialize)? != expected {
            return Err("reopen mismatch".into());
        }
        db.execute_batch(
            "ALTER TABLE circuit_view RENAME TO reopened_view;BEGIN;DELETE FROM a;ROLLBACK;",
        )?;
        if rows(&db, "SELECT * FROM reopened_view")? != expected {
            return Err("rename/rollback mismatch".into());
        }
        db.execute_batch("DROP TABLE reopened_view")?;
    }
    println!(
        "{}",
        json!({"event":"case-total","status":"ok","update_plus_query_ms":total,"final_input_hash":input_hash,"final_checksum":checksum,"disk":{"database_bytes":std::fs::metadata(&path)?.len(),"wal_bytes":std::fs::metadata(format!("{}-wal",path.display())).map(|m|m.len()).unwrap_or(0)}})
    );
    Ok(())
}
