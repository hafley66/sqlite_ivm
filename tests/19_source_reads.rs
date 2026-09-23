#![cfg(not(feature = "extension"))]
use hafley_observe::{
    sqlite::{instrument, silence, SQLITE_TARGET},
    CountRecorder,
};
use rusqlite::{Connection, Result};
use sqlite_ivm::extension::register;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// One changed key, increasing unrelated source rows. SQL, plans and execution
/// counters come from the existing observe APIs.
#[test]
fn maintenance_source_read_costs() -> Result<()> {
    for size in [32, 1024, 8192] {
        for shape in ["join_group", "nested_union"] {
            let db = Connection::open_in_memory()?;
            register(&db)?;
            db.execute_batch(&format!("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
                CREATE TABLE source_rows(k INTEGER PRIMARY KEY,v INTEGER);
                CREATE TABLE dimension_rows(k INTEGER PRIMARY KEY);
                WITH RECURSIVE r(i) AS(SELECT 1 UNION ALL SELECT i+1 FROM r WHERE i<{size})
                INSERT INTO source_rows SELECT i,i FROM r;
                INSERT INTO dimension_rows SELECT k FROM source_rows"))?;
            let query = if shape == "join_group" {
                "SELECT s.k,sum(s.v) AS v FROM source_rows s JOIN dimension_rows d ON s.k=d.k GROUP BY s.k".to_owned()
            } else {
                let mut definitions = vec!["r0 AS (SELECT k,v FROM source_rows)".to_owned()];
                for depth in 1..4 {
                    definitions.push(format!("r{depth} AS (SELECT k,v FROM r{} UNION SELECT k,v FROM r{})",depth-1,depth-1));
                }
                format!("WITH {} SELECT k,v FROM r3", definitions.join(","))
            };
            db.execute_batch(&format!("CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
            let (recorder, layer) = CountRecorder::new();
            let guard = tracing_subscriber::registry().with(layer).set_default();
            instrument(&db);
            db.execute_batch("UPDATE source_rows SET v=v+1 WHERE k=1")?;
            silence(&db);
            drop(guard);
            let read = |sql: &str| -> Result<Vec<(i64,i64)>> {
                db.prepare(sql)?.query_map([], |row| Ok((row.get(0)?,row.get(1)?)))?.collect()
            };
            assert_eq!(read("SELECT * FROM result ORDER BY 1,2")?,read(&format!("{query} ORDER BY 1,2"))?);
            let statements = recorder.event_sums(SQLITE_TARGET,tracing::Level::DEBUG,"drain","view",Some("sql"));
            if shape == "nested_union" {
                let vm_steps: f64 = statements.iter().filter(|((view,_),_)|view=="result")
                    .map(|(_,sums)|sums.sum_of("vm_step")).sum();
                assert!(vm_steps <= 6500.0,
                    "one-key UNION work grew with {size} source rows: {vm_steps} VM steps");
            }
            for ((view,sql),sums) in statements {
                if view != "result" { continue; }
                if shape == "join_group" && sql.contains("sum(CASE") {
                    assert_eq!(sums.events,1);
                    assert_eq!(sums.sum_of("run"),1.0);
                    assert!(sums.sum_of("vm_step") <= 150.0,
                        "one touched group scanned unrelated keys at size {size}: {sums:?}");
                }
                let parameter_count = db.prepare(&sql)?.parameter_count();
                let plan = if parameter_count == 0 {
                    hafley_observe::sqlite::query_plan(&db,&sql)?
                } else { Vec::new() };
                println!("MAINTENANCE_AUDIT {}",serde_json::json!({
                    "source_rows":size,"shape":shape,
                    "counter_scope":"statement lifetime; sums can include earlier cached executions",
                    "single_execution_counters":sums.events == 1 && sums.sum_of("run") == 1.0,
                    "mutation":"UPDATE source_rows SET v=v+1 WHERE k=1",
                    "query":query,"sql":sql,"plan":plan,"plan_skipped_parameters":parameter_count,
                    "executions":sums.events,"counters":sums.sums
                }));
            }
        }
    }
    Ok(())
}

#[test]
fn union_membership_applies_both_input_deltas_before_emitting() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE a(x INTEGER);CREATE TABLE b(x INTEGER);
        INSERT INTO a VALUES(1),(1),(2);INSERT INTO b VALUES(1),(3)")?;
    let query = "SELECT x FROM a UNION SELECT x FROM b";
    db.execute_batch(&format!("CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
    let rows = |sql: &str| -> Result<Vec<i64>> {
        db.prepare(sql)?.query_map([],|r|r.get(0))?.collect()
    };
    assert_eq!(rows("SELECT x FROM result ORDER BY x")?,rows(&format!("{query} ORDER BY x"))?);
    for mutation in [
        "BEGIN;DELETE FROM a WHERE x=1;INSERT INTO b VALUES(2);COMMIT",
        "BEGIN;DELETE FROM b WHERE x=1;INSERT INTO a VALUES(1);COMMIT",
        "BEGIN;DELETE FROM a WHERE x=1;DELETE FROM b WHERE x=2;COMMIT",
        "BEGIN;SAVEPOINT s;INSERT INTO a VALUES(4);INSERT INTO b VALUES(4);ROLLBACK TO s;RELEASE s;COMMIT",
        "BEGIN;INSERT INTO a VALUES(5);INSERT INTO b VALUES(5);COMMIT",
        "BEGIN;DELETE FROM a WHERE x=5;DELETE FROM b WHERE x=5;COMMIT",
    ] {
        db.execute_batch(mutation)?;
        assert_eq!(rows("SELECT x FROM result ORDER BY x")?,rows(&format!("{query} ORDER BY x"))?,"{mutation}");
    }
    Ok(())
}

#[test]
fn distinct_membership_replaces_a_deleted_representative() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE a(x TEXT COLLATE NOCASE);INSERT INTO a VALUES('AA'),('aa')")?;
    db.execute_batch("CREATE VIRTUAL TABLE result USING sqlite_ivm('SELECT DISTINCT x FROM a')")?;
    let actual = || -> Result<Vec<String>> {
        db.prepare("SELECT x FROM result")?.query_map([],|r|r.get(0))?.collect()
    };
    assert_eq!(actual()?,vec!["AA"]);
    db.execute_batch("DELETE FROM a WHERE x='AA' COLLATE BINARY")?;
    assert_eq!(actual()?,vec!["aa"]);
    db.execute_batch("INSERT INTO a VALUES('Aa')")?;
    assert_eq!(actual()?,vec!["aa"]);
    db.execute_batch("DELETE FROM a WHERE x='aa' COLLATE BINARY")?;
    assert_eq!(actual()?,vec!["Aa"]);
    Ok(())
}

#[test]
fn union_membership_switches_representative_between_inputs() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE a(x TEXT COLLATE NOCASE);CREATE TABLE b(x TEXT COLLATE NOCASE);
        INSERT INTO b VALUES('alpha');
        CREATE VIRTUAL TABLE result USING sqlite_ivm('SELECT x FROM a UNION SELECT x FROM b')")?;
    let result = || -> Result<String> { db.query_row("SELECT x FROM result",[],|r|r.get(0)) };
    assert_eq!(result()?,"alpha");
    db.execute_batch("INSERT INTO a VALUES('ALPHA')")?;
    assert_eq!(result()?,"ALPHA");
    db.execute_batch("DELETE FROM a WHERE x='ALPHA' COLLATE BINARY")?;
    assert_eq!(result()?,"alpha");
    Ok(())
}

#[test]
fn format_eight_set_view_rebuilds_membership_on_connect() -> Result<()> {
    let path = std::env::temp_dir().join(format!("ivm-set-format8-{}.db",std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let db = Connection::open(&path)?;
        register(&db)?;
        db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
            CREATE TABLE a(x INTEGER);INSERT INTO a VALUES(1),(1);
            CREATE VIRTUAL TABLE result USING sqlite_ivm('SELECT DISTINCT x FROM a')")?;
        let table: String = db.query_row("SELECT object_name FROM __ivm_objects WHERE view_name='result' AND object_type='table' AND object_name LIKE 'result_op%x0'",[],|r|r.get(0))?;
        db.execute("DELETE FROM __ivm_objects WHERE object_name=?1",[&table])?;
        db.execute_batch(&format!("DROP TABLE main.{}",sqlite_ivm::catalog::quote(&table)))?;
        db.execute_batch("UPDATE __ivm_schema SET format_version=8")?;
    }
    {
        let db = Connection::open(&path)?;
        register(&db)?;
        db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON")?;
        let value: i64 = db.query_row("SELECT x FROM result",[],|r|r.get(0))?;
        assert_eq!(value,1);
        let format: i64 = db.query_row("SELECT format_version FROM __ivm_schema",[],|r|r.get(0))?;
        assert_eq!(format,11);
        db.execute_batch("INSERT INTO a VALUES(2)")?;
        assert_eq!(db.query_row("SELECT count(*) FROM result",[],|r|r.get::<_,i64>(0))?,2);
    }
    std::fs::remove_file(path).ok();
    Ok(())
}

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
    assert!(plans.iter().any(|step| step == "SCAN changed"), "{plans:#?}");
    assert!(plans.iter().any(|step| step == "SEARCH d USING INTEGER PRIMARY KEY (rowid=?)"), "{plans:#?}");
    assert!(!plans.iter().any(|step| step.starts_with("SEARCH main.result_keys USING") && step.ends_with("(__node=?)")), "{plans:#?}");
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
    let native: i64 = db.query_row("SELECT count(*) FROM result_keys", [], |r| r.get(0))?;
    assert_eq!(native, 3);
    let text_key: i64 = db.query_row("SELECT count(*) FROM pragma_table_info('result_keys') WHERE name='__v'", [], |r| r.get(0))?;
    assert_eq!(text_key, 0);
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
fn set_source_reads_use_indexed_membership_boundaries() -> Result<()> {
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
    let json_indexes: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='index' AND name LIKE '__ivm_%' AND (sql LIKE '%json_array(%' OR sql LIKE '%json_object(%')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(json_indexes, 0);
    assert!(statements.keys().all(|(_, sql)| !sql.contains("json_array(") && !sql.contains("json_object(")));
    let mut indexed_membership_count = 0;
    for ((_, sql), sums) in &statements {
        if !sql.contains("result_op") || !sql.contains("__ivm_touched") || sql.contains('?') {
            continue;
        }
        let plan = hafley_observe::sqlite::query_plan(&db, sql)?;
        assert!(!plan.iter().any(|step| step.starts_with("MATERIALIZE __ivm_read_")), "{plan:?}");
        assert!(!sql.contains("__ivm_read_"));
        indexed_membership_count += sums.events;
    }
    assert!(indexed_membership_count > 0, "set state did not probe touched keys");
    // Profile mem_used is summed over repeated executions for each SQL key.
    let total_accumulated_mem_used = statements
        .values()
        .map(|sums| sums.sum_of("mem_used"))
        .sum::<f64>();
    let max_accumulated_mem_used = statements
        .values()
        .map(|sums| sums.sum_of("mem_used"))
        .fold(0.0, f64::max);
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
    // Every source edit reaches both branches of each nested union.
    for mutation in [
        "BEGIN;INSERT INTO source_rows VALUES(4),(NULL),(5)",
        "UPDATE source_rows SET k=6 WHERE k=2",
        "SAVEPOINT nested_union;DELETE FROM source_rows WHERE k=4",
        "ROLLBACK TO nested_union;RELEASE nested_union",
        "DELETE FROM source_rows WHERE k=1;COMMIT",
    ] {
        db.execute_batch(mutation)?;
        let read = |sql: &str| -> Result<Vec<rusqlite::types::Value>> {
            db.prepare(sql)?.query_map([], |row| row.get(0))?.collect()
        };
        assert_eq!(
            read("SELECT k FROM result ORDER BY k")?,
            read(&format!("SELECT k FROM ({query}) ORDER BY k"))?,
            "{mutation}"
        );
    }
    Ok(())
}

#[test]
fn recursive_rules_read_current_union_membership_after_retraction() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
         CREATE TABLE left_roots(n INTEGER);CREATE TABLE right_roots(n INTEGER);
         CREATE TABLE edges(src INTEGER,dst INTEGER);
         INSERT INTO left_roots VALUES(1);INSERT INTO right_roots VALUES(2);
         INSERT INTO edges VALUES(1,3),(2,3),(3,4)",
    )?;
    let query = "WITH RECURSIVE seeds(n) AS (SELECT n FROM left_roots UNION SELECT n FROM right_roots),
        reach(n) AS (SELECT n FROM seeds UNION SELECT e.dst FROM reach r JOIN edges e ON e.src=r.n)
        SELECT n FROM reach";
    db.execute_batch(&format!("CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
    let rows = |sql: &str| -> Result<Vec<i64>> {
        db.prepare(sql)?.query_map([], |row| row.get(0))?.collect()
    };
    for mutation in [
        "DELETE FROM left_roots WHERE n=1",
        "BEGIN;INSERT INTO left_roots VALUES(5);INSERT INTO edges VALUES(5,6);COMMIT",
        "BEGIN;DELETE FROM right_roots WHERE n=2;DELETE FROM edges WHERE src=5;COMMIT",
        "INSERT INTO right_roots VALUES(1)",
    ] {
        db.execute_batch(mutation)?;
        assert_eq!(
            rows("SELECT n FROM result ORDER BY n")?,
            rows(&format!("{query} ORDER BY n"))?,
            "{mutation}"
        );
    }
    Ok(())
}

#[test]
fn population_reuses_completed_operator_results() -> Result<()> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;
        CREATE TABLE a(k INTEGER,v);CREATE TABLE b(k INTEGER);
        INSERT INTO a VALUES(1,7),(2,9);INSERT INTO b VALUES(1),(2)")?;
    let mut definitions = vec!["r0 AS (SELECT k,v FROM a)".to_owned()];
    for depth in 1..7 {
        definitions.push(format!("r{depth} AS (SELECT l.k,l.v FROM r{} l JOIN r{} r ON l.k=r.k UNION SELECT a.k,a.v FROM a JOIN b ON a.k=b.k)", depth-1, depth-1));
    }
    let query = format!("WITH {} SELECT k,v FROM r6", definitions.join(","));
    let (recorder, layer) = CountRecorder::new();
    let guard = tracing_subscriber::registry().with(layer).set_default();
    instrument(&db);
    db.execute_batch(&format!("CREATE VIRTUAL TABLE result USING sqlite_ivm('{query}')"))?;
    silence(&db);
    drop(guard);
    let issued = recorder.event_sums(SQLITE_TARGET, tracing::Level::DEBUG, "populate_node", "view", Some("sql"));
    let population = issued.keys().filter(|(view, _)| view == "result").map(|(_, sql)| sql).collect::<Vec<_>>();
    assert!(!population.is_empty());
    let largest = population.iter().map(|sql| sql.len()).max().unwrap();
    assert!(largest < 12_000, "population statement expanded to {largest} bytes");
    for sql in population {
        assert!(!sql.contains("__ivm_read_"), "population traversed upstream graph: {sql}");
    }
    let scratch = db.prepare("SELECT name FROM temp.sqlite_schema WHERE type='table' AND name GLOB '__ivm_out_*'")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>>>()?;
    for table in scratch {
        let remaining: i64 = db.query_row(&format!("SELECT count(*) FROM temp.{}", sqlite_ivm::catalog::quote(&table)), [], |row| row.get(0))?;
        assert_eq!(remaining, 0, "population retained completed rows in {table}");
    }
    let read = |sql: &str| -> Result<Vec<(rusqlite::types::Value,rusqlite::types::Value)>> {
        db.prepare(sql)?.query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect()
    };
    assert_eq!(read("SELECT * FROM result ORDER BY 1,2")?, read(&format!("{query} ORDER BY 1,2"))?);
    // Both inputs change at one transaction boundary, then rollback restores them.
    for mutation in [
        "BEGIN;INSERT INTO a VALUES(3,x'00');INSERT INTO b VALUES(3);COMMIT",
        "BEGIN;SAVEPOINT s;DELETE FROM a WHERE k=1;DELETE FROM b WHERE k=1;RELEASE s;ROLLBACK",
        "BEGIN;UPDATE a SET v=11 WHERE k=2;DELETE FROM b WHERE k=2;COMMIT",
    ] {
        db.execute_batch(mutation)?;
        assert_eq!(read("SELECT * FROM result ORDER BY 1,2")?,read(&format!("{query} ORDER BY 1,2"))?,"{mutation}");
    }
    Ok(())
}
