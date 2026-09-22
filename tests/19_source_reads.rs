#![cfg(not(feature = "extension"))]
use hafley_observe::{
    sqlite::{instrument, silence, SQLITE_TARGET},
    CountRecorder,
};
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[test]
fn join_deltas_probe_sources_and_never_write_input_copies() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE fact(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);
        CREATE TABLE dimension(id INTEGER PRIMARY KEY,k INTEGER,factor INTEGER);
        WITH RECURSIVE r(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM r WHERE i<128)
        INSERT INTO fact SELECT i,i%8,i FROM r;
        INSERT INTO dimension VALUES(1,1,3),(2,1,4),(3,2,5)",
    )?;
    let query = "SELECT f.k,count(*) AS n,sum(f.v*d.factor) AS s FROM fact f JOIN dimension d ON f.k=d.k GROUP BY f.k";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"
    ))?;
    let copied: Vec<String> = db.prepare("SELECT object_name FROM __ivm_objects WHERE view_name='result' AND object_type='table' AND EXISTS(SELECT 1 FROM pragma_table_info(object_name) WHERE name='__r')")?
        .query_map([],|r|r.get(0))?.collect::<Result<_>>()?;
    assert_eq!(copied, Vec::<String>::new());

    let (recorder, layer) = CountRecorder::new();
    let guard = tracing_subscriber::registry().with(layer).set_default();
    instrument(&db);
    db.execute_batch("BEGIN;UPDATE fact SET v=v+1 WHERE id=1;UPDATE dimension SET factor=factor+1 WHERE id=1;COMMIT")?;
    silence(&db);
    drop(guard);
    let read = |sql: &str| -> Result<Vec<(i64, i64, i64)>> {
        db.prepare(sql)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
    };
    assert_eq!(
        read("SELECT * FROM result ORDER BY 1")?,
        read(&format!("{query} ORDER BY 1"))?
    );
    let statements = recorder.event_sums(
        SQLITE_TARGET,
        tracing::Level::DEBUG,
        "drain",
        "view",
        Some("sql"),
    );
    let mut plans = Vec::new();
    for (_, sql) in statements.keys() {
        for encoding in [
            "json_array(",
            "json_object(",
            "json_each(",
            "sqlite_ivm_hash(",
        ] {
            assert!(
                !sql.contains(encoding),
                "serialized join/group/result key: {sql}"
            );
        }
        if sql.starts_with("UPDATE main.\"result_state\"")
            || sql.starts_with("DELETE FROM main.\"result_state\"")
        {
            let update_plan = hafley_observe::sqlite::query_plan(&db, sql)?;
            assert!(
                update_plan
                    .iter()
                    .any(|p| p.contains("INTEGER PRIMARY KEY (rowid=?)")),
                "{update_plan:#?}"
            );
            assert!(
                !update_plan.iter().any(|p| p.starts_with("SCAN s")),
                "{update_plan:#?}"
            );
        }
        if sql.starts_with("INSERT INTO temp.__ivm_out_") && sql.contains(" JOIN ") {
            plans.extend(hafley_observe::sqlite::query_plan(&db, sql)?);
        }
        assert!(
            !sql.starts_with("INSERT INTO main.\"result_op") || sql.contains("__safe"),
            "{sql}"
        );
        assert!(!sql.starts_with("UPDATE main.\"result_op"), "{sql}");
    }
    for source in ["fact", "dimension"] {
        assert!(
            plans
                .iter()
                .any(|p| p.contains(source) && p.contains("USING INDEX __ivm_result_source")),
            "{source}: {plans:#?}"
        );
    }
    Ok(())
}

#[test]
fn composite_keys_use_native_cells_and_group_rowids_survive_updates() -> Result<()> {
    use rusqlite::types::Value;
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE fact(id INTEGER PRIMARY KEY,k,tag,v INTEGER);
        CREATE TABLE dimension(id INTEGER PRIMARY KEY,k,tag,factor INTEGER);
        INSERT INTO fact VALUES(1,1,x'00',3),(2,1.0,x'00',3),(3,'1',x'00',7),
            (4,NULL,x'00',11),(5,2,'text',13),(6,2,'text',17);
        INSERT INTO dimension VALUES(1,1,x'00',2),(2,'1',x'00',5),
            (3,NULL,x'00',19),(4,2,'text',3)",
    )?;
    let query = "SELECT f.k,f.tag,count(*) AS n,sum(f.v*d.factor) AS s FROM fact f JOIN dimension d ON f.k=d.k AND f.tag=d.tag GROUP BY f.k,f.tag";
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"
    ))?;
    let rows = |sql: &str| -> Result<Vec<Vec<Value>>> {
        let mut s = db.prepare(sql)?;
        let width = s.column_count();
        let values = s
            .query_map([], |r| (0..width).map(|i| r.get(i)).collect())?
            .collect();
        values
    };
    let ids = rows("SELECT rowid,k,tag FROM result ORDER BY k,tag")?;
    let indexes: Vec<String> = db
        .prepare(
            "SELECT definition FROM __ivm_objects WHERE view_name='result' AND object_type='index'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let indexed_columns: Vec<(String,String)> = db.prepare("SELECT s.tbl_name,group_concat(p.name,',') FROM sqlite_schema s JOIN pragma_index_info(s.name) p WHERE s.type='index' AND s.tbl_name IN ('fact','dimension') GROUP BY s.name")?
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_>>()?;
    for source in ["fact", "dimension"] {
        assert!(
            indexed_columns.contains(&(source.into(), "k,tag".into())),
            "{indexed_columns:#?}"
        );
    }
    assert!(
        indexes
            .iter()
            .all(|s| !s.contains("json_") && !s.contains("sqlite_ivm_hash")),
        "{indexes:#?}"
    );
    let native: (i64, i64) =
        db.query_row("SELECT count(*),count(__v) FROM result_keys", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    assert_eq!(native, (3, 0));
    for mutation in [
        "BEGIN;UPDATE fact SET v=v+1 WHERE id=1;UPDATE dimension SET factor=factor+1 WHERE id=1;COMMIT",
        "BEGIN;UPDATE fact SET k=2,tag='text' WHERE id IN(1,2);DELETE FROM dimension WHERE id=4;ROLLBACK",
        "UPDATE fact SET v=v-2 WHERE id=3",
        "VACUUM",
    ] {
        db.execute_batch(mutation)?;
        assert_eq!(rows("SELECT * FROM result ORDER BY k,tag")?,rows(&format!("{query} ORDER BY f.k,f.tag"))?,"{mutation}");
        assert_eq!(rows("SELECT rowid,k,tag FROM result ORDER BY k,tag")?,ids,"stable group rowids: {mutation}");
    }
    // A checksum collision must never select rows or conflate duplicate bags.
    db.create_scalar_function(
        c"sqlite_ivm_row_check",
        -1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |_| Ok(0i64),
    )?;
    db.execute_batch("UPDATE result_state SET __check=0")?;
    db.execute_batch("DELETE FROM fact WHERE id=1;DELETE FROM fact WHERE k='1'")?;
    assert_eq!(
        rows("SELECT * FROM result ORDER BY k,tag")?,
        rows(&format!("{query} ORDER BY f.k,f.tag"))?
    );
    Ok(())
}

#[test]
fn shared_subqueries_keep_statement_text_bounded() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE source_rows(k INTEGER);INSERT INTO source_rows VALUES(1),(1),(2);
        CREATE TABLE dimension_rows(k INTEGER PRIMARY KEY);INSERT INTO dimension_rows VALUES(1),(2),(3)")?;
    let mut definitions = vec!["r0 AS (SELECT k FROM source_rows)".to_string()];
    for depth in 1..6 {
        definitions.push(format!(
            "r{depth} AS (SELECT a.k FROM r{} a LEFT JOIN dimension_rows b ON a.k=b.k)",
            depth - 1
        ));
    }
    let query = format!("WITH {} SELECT k FROM r5", definitions.join(","));
    let (recorder, layer) = CountRecorder::new();
    let guard = tracing_subscriber::registry().with(layer).set_default();
    instrument(&db);
    db.execute_batch(&format!(
        "CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"
    ))?;
    for mutation in [
        "INSERT INTO source_rows VALUES(3)",
        "DELETE FROM source_rows WHERE k=1",
    ] {
        db.execute_batch(mutation)?;
        let read = |sql: &str| -> Result<Vec<i64>> {
            db.prepare(sql)?.query_map([], |r| r.get(0))?.collect()
        };
        assert_eq!(
            read("SELECT k FROM result ORDER BY k")?,
            read(&format!("{query} ORDER BY k"))?
        );
    }
    silence(&db);
    drop(guard);
    let statements = recorder.event_sums(
        SQLITE_TARGET,
        tracing::Level::DEBUG,
        "drain",
        "view",
        Some("sql"),
    );
    let largest = statements
        .keys()
        .map(|(_, sql)| sql.len())
        .max()
        .unwrap_or(0);
    println!("largest SQL statement: {largest} bytes");
    assert!(
        largest > 0 && largest < 20_000,
        "largest SQL statement: {largest} bytes"
    );
    Ok(())
}

#[test]
fn set_source_reads_keep_materialized_boundaries_observable() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE source_rows(k INTEGER);INSERT INTO source_rows VALUES(1),(2),(3);",
    )?;
    let mut definitions = vec!["r0 AS (SELECT k FROM source_rows)".to_string()];
    for depth in 1..6 {
        definitions.push(format!(
            "r{depth} AS (SELECT k FROM r{} UNION SELECT k FROM r{})",
            depth - 1,
            depth - 1
        ));
    }
    let query = format!("WITH {} SELECT k FROM r5", definitions.join(","));
    let (recorder, layer) = CountRecorder::new();
    let guard = tracing_subscriber::registry().with(layer).set_default();
    instrument(&db);
    db.execute_batch(&format!("CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
    db.execute_batch("INSERT INTO source_rows VALUES(4)")?;
    silence(&db);
    drop(guard);
    let statements = recorder.event_sums(
        SQLITE_TARGET,
        tracing::Level::DEBUG,
        "drain",
        "view",
        Some("sql"),
    );
    let mut materialized_count = 0;
    let mut prepared_bytes = 0.0;
    for ((_, sql), sums) in &statements {
        if !sql.starts_with("INSERT INTO temp.__ivm_out_") || sql.contains('?') {
            continue;
        }
        let plan = hafley_observe::sqlite::query_plan(&db, sql)?;
        if plan
            .iter()
            .any(|step| step.starts_with("MATERIALIZE __ivm_read_"))
        {
            materialized_count += 1;
            prepared_bytes += sums.sum_of("mem_used");
        }
    }
    assert!(materialized_count > 0, "set boundary was inlined");
    // Profile mem_used is summed over repeated executions for each SQL key.
    let total_accumulated_mem_used = statements
        .values()
        .map(|sums| sums.sum_of("mem_used"))
        .sum::<f64>();
    let max_accumulated_mem_used = statements
        .values()
        .map(|sums| sums.sum_of("mem_used"))
        .fold(0.0, f64::max);
    assert!(prepared_bytes < 6_000_000.0, "prepared set boundary: {prepared_bytes}");
    assert!(
        total_accumulated_mem_used < 10_000_000.0,
        "all accumulated prepared statements: {total_accumulated_mem_used}"
    );
    assert!(
        max_accumulated_mem_used < 1_500_000.0,
        "largest accumulated prepared SQL key: {max_accumulated_mem_used}"
    );
    assert_eq!(
        db.prepare("SELECT k FROM result ORDER BY k")?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>>>()?,
        vec![1, 2, 3, 4]
    );
    Ok(())
}
