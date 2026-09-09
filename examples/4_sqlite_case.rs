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
fn metric(value: Option<u64>, unit: &str, reason: Option<&str>) -> Value {
    json!({"value":value,"unit":unit,"unavailable_reason":reason})
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn state_inventory(db: &Connection, path: &PathBuf, maintained: bool) -> Result<Value> {
    let mut allocations = std::collections::BTreeMap::new();
    let dbstat_reason =
        match db.prepare("SELECT name,sum(pgsize),sum(payload) FROM dbstat GROUP BY name") {
            Ok(mut statement) => match statement.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            }) {
                Ok(mapped) => {
                    for item in mapped {
                        let (name, allocated, payload) = item?;
                        allocations.insert(name, (allocated as u64, payload as u64));
                    }
                    None
                }
                Err(error) => Some(format!("SQLite dbstat unavailable: {error}")),
            },
            Err(error) => Some(format!("SQLite dbstat unavailable: {error}")),
        };
    let mut owned = std::collections::BTreeSet::new();
    if maintained {
        let mut statement = db.prepare("SELECT object_type,object_name FROM __ivm_objects WHERE view_name='circuit_view' AND object_type IN ('table','index')")?;
        for item in
            statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            owned.insert(item?);
        }
    }
    let owned_tables = owned
        .iter()
        .filter(|(kind, _)| kind == "table")
        .map(|(_, name)| name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let catalogs = [
        "__ivm_schema",
        "__ivm_views",
        "__ivm_sources",
        "__ivm_columns",
        "__ivm_objects",
    ];
    let mut relations = Vec::new();
    let mut statement = db.prepare("SELECT type,name,tbl_name,rootpage FROM sqlite_schema WHERE type IN ('table','index') ORDER BY type,name")?;
    let catalog = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    for (kind, name, table_name, rootpage) in catalog {
        if kind == "table" && name.starts_with("sqlite_") {
            continue;
        }
        let role = if kind == "table" {
            if ["a", "b", "c"].contains(&name.as_str()) {
                "source"
            } else if owned.contains(&(kind.clone(), name.clone())) {
                if name.ends_with("_result") || name.ends_with("_state") {
                    "result"
                } else {
                    "support"
                }
            } else if catalogs.contains(&name.as_str()) {
                "catalog"
            } else {
                continue;
            }
        } else if ["a", "b", "c"].contains(&table_name.as_str()) {
            "source-index"
        } else if owned.contains(&(kind.clone(), name.clone()))
            || owned_tables.contains(&table_name)
        {
            "support-index"
        } else if catalogs.contains(&table_name.as_str()) {
            "catalog-index"
        } else {
            continue;
        };
        let row_count = if kind == "table" {
            Some(db.query_row(
                &format!("SELECT count(*) FROM main.{}", quote(&name)),
                [],
                |r| r.get::<_, i64>(0),
            )? as u64)
        } else {
            None
        };
        let allocation = allocations.get(&name).copied();
        let missing = dbstat_reason.as_deref().unwrap_or(if rootpage == 0 {
            "relation has no physical root page"
        } else {
            "relation has no dbstat pages"
        });
        relations.push(json!({"name":name,"kind":kind,"role":role,"counted_in_totals":true,
            "row_count":metric(row_count,"rows",if row_count.is_none(){Some("row count does not apply to an index")}else{None}),
            "bytes":{"allocated":metric(allocation.map(|x|x.0),"bytes",if allocation.is_none(){Some(missing)}else{None}),
                     "data":metric(allocation.map(|x|x.1),"bytes",if allocation.is_none(){Some(missing)}else{None}),
                     "index":metric(if kind=="index"{allocation.map(|x|x.0)}else{None},"bytes",if kind=="table"{Some("index allocation is reported on separate index relations")}else if allocation.is_none(){Some(missing)}else{None})}}));
    }
    fn aggregate(relations: &[Value], kind: &str, field: &str) -> Value {
        let selected = relations
            .iter()
            .filter(|r| r["kind"] == kind)
            .collect::<Vec<_>>();
        let values = selected
            .iter()
            .map(|r| {
                if field == "rows" {
                    r["row_count"]["value"].as_u64()
                } else {
                    r["bytes"]["allocated"]["value"].as_u64()
                }
            })
            .collect::<Vec<_>>();
        if values.iter().any(Option::is_none) {
            json!({"value":null,"unit":if field=="rows"{"rows"}else{"bytes"},"unavailable_reason":"one or more included relations are unavailable","partial":true,"known_value":values.into_iter().flatten().sum::<u64>()})
        } else {
            json!({"value":values.into_iter().flatten().sum::<u64>(),"unit":if field=="rows"{"rows"}else{"bytes"},"unavailable_reason":null,"partial":false})
        }
    }
    let table_bytes = aggregate(&relations, "table", "bytes");
    let index_bytes = aggregate(&relations, "index", "bytes");
    let total_relation = match (table_bytes["value"].as_u64(), index_bytes["value"].as_u64()) {
        (Some(a), Some(b)) => {
            json!({"value":a+b,"unit":"bytes","unavailable_reason":null,"partial":false})
        }
        _ => {
            json!({"value":null,"unit":"bytes","unavailable_reason":"table or index allocation unavailable","partial":true})
        }
    };
    let page_count = db.query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))? as u64;
    let page_size = db.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))? as u64;
    let mut rows_by_role = serde_json::Map::new();
    for role in ["source", "result", "support", "catalog"] {
        let selected = relations
            .iter()
            .filter(|r| r["kind"] == "table" && r["role"] == role)
            .cloned()
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            rows_by_role.insert(role.into(), aggregate(&selected, "table", "rows"));
        }
    }
    let database_file = std::fs::metadata(path);
    let wal_path = format!("{}-wal", path.display());
    let wal_file = std::fs::metadata(&wal_path);
    Ok(
        json!({"schema_version":1,"measured_at":"after-output-validation","outside_timed_region":true,
      "scope":"SQLite source relations and indexes plus sqlite_ivm-owned result/support/catalog relations; unrelated relations excluded","relations":relations,
      "summary":{"table_count":metric(Some(relations.iter().filter(|r|r["kind"]=="table").count() as u64),"tables",None),"index_count":metric(Some(relations.iter().filter(|r|r["kind"]=="index").count() as u64),"indexes",None),"native_collection_count":metric(None,"collections",Some("SQL adapter has no native collection inventory")),"total_rows":aggregate(&relations,"table","rows"),"rows_by_role":rows_by_role,"table_bytes":table_bytes,"index_bytes":index_bytes,"total_relation_bytes":total_relation},
      "storage":{"database_file_bytes":metric(database_file.as_ref().ok().map(std::fs::Metadata::len),"bytes",database_file.as_ref().err().map(|_|"database filesystem metadata unavailable")),"wal_file_bytes":metric(match &wal_file{Ok(m)=>Some(m.len()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Some(0),Err(_)=>None},"bytes",match &wal_file{Err(e) if e.kind()!=std::io::ErrorKind::NotFound=>Some("WAL filesystem metadata unavailable"),_=>None}),"database_allocated_bytes":metric(Some(page_count*page_size),"bytes",None),"database_size_scope":"SQLite main database page allocation; database and WAL filesystem lengths are separate"},
      "process_memory":{"rss_bytes":metric(None,"bytes",Some("measured by the parent runner as process peak RSS, outside this snapshot"))},
      "limitations":if maintained{Vec::<&str>::new()}else{vec!["plain query has no durable result relation; output_bytes is transient serialized query output"]}}),
    )
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
        let inventory = state_inventory(&db, &path, extension.is_some())?;
        println!(
            "{}",
            json!({"event":"mutation","status":"ok","state":state["name"],"exact_input_output_validated":true,"input_hash":input_hash,"checksum":checksum,"affected_rows":state["writes"].as_array().unwrap().len(),"output_rows":actual.len(),"output_bytes":canonical.len(),"update_transaction_ms":update,"query_compute_ms":compute,"update_plus_query_ms":update+compute,"state_inventory":inventory})
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
