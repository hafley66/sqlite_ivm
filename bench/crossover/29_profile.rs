//! Diagnostic replay. Capture costs are excluded from performance claims.
use anyhow::{ensure, Result};
use hafley_observe::{CountRecorder, process_sample};
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Instant};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use sha2::{Digest, Sha256};

const QUERY: &str = "SELECT f.group_id,COUNT(*) AS n,SUM(f.amount*d.factor) AS s FROM fact f JOIN dimension d ON f.group_id=d.group_id GROUP BY f.group_id";

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<i64>>> {
    let mut statement = db.prepare(sql)?;
    let width = statement.column_count();
    let mut rows = statement.query_map([], |row| (0..width).map(|i| row.get(i)).collect())?
        .collect::<rusqlite::Result<Vec<Vec<i64>>>>()?;
    rows.sort();
    Ok(rows)
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(args.len() >= 3, "crossover-profile <fixture.json> <new-output-dir> <current|historical> [historical-library]");
    let fixture: Value = serde_json::from_slice(&fs::read(&args[0])?)?;
    let out = PathBuf::from(&args[1]);
    fs::create_dir(&out)?;
    let historical = args[2] == "historical";
    ensure!(historical || args[2] == "current", "unknown arm");
    let db = Connection::open(out.join("state.db"))?;
    db.execute_batch("PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA cache_size=-8192;
        CREATE TABLE fact(id INTEGER PRIMARY KEY,group_id INTEGER NOT NULL,amount INTEGER NOT NULL);
        CREATE TABLE dimension(group_id INTEGER PRIMARY KEY,factor INTEGER NOT NULL);")?;
    if historical {
        ensure!(args.len() == 4, "historical library required");
        unsafe { db.load_extension_enable()?; db.load_extension(&args[3], None::<&str>)?; }
        db.load_extension_disable()?;
        db.execute_batch("SELECT take2_control('cache_on');SELECT take2_control('source_views_on');
            CREATE INDEX fact_group_idx ON fact(group_id);
            CREATE VIRTUAL TABLE native_result USING take2_counted(join);
            CREATE VIEW summary AS SELECT id AS group_id,k AS n,v AS s FROM native_result;
            SELECT take2_attach('native_result','fact',0,'id','group_id','amount');
            SELECT take2_attach('native_result','dimension',1,'group_id','group_id','factor');
            SELECT take2_prepare('native_result');")?;
    } else {
        sqlite_ivm::extension::register(&db)?;
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    for relation in ["dimension", "fact"] {
        let input = fixture["states"][0]["inputs"][relation].as_array().unwrap();
        let mut insert = db.prepare(&format!("INSERT INTO {relation} VALUES({})", vec!["?"; input[0].as_array().unwrap().len()].join(",")))?;
        for row in input {
            insert.execute(params_from_iter(row.as_array().unwrap().iter().map(|x| x.as_i64().unwrap())))?;
        }
    }
    db.execute_batch("COMMIT")?;
    if !historical { db.query_row("SELECT sqlite_ivm_create('summary',?1)", [QUERY], |_| Ok(()))?; }
    // Install after initial population, keeping capture storage bounded to one
    // mutation at a time. Each state owns a fresh recorder and its SQL trace.
    let mut report = vec![];
    for state in fixture["states"].as_array().unwrap().iter().skip(1) {
        let name = state["name"].as_str().unwrap();
        std::env::set_var("HAFLEY_TRACE", out.join(format!("{name}.trace.json")));
        let log = hafley_observe::sqlite::open(&out.join(format!("{name}.events.db")), hafley_observe::sqlite::Encoding::Dictionary, hafley_observe::Flush::OnCommit)?;
        let (recorder, capture) = CountRecorder::new();
        let subscriber = tracing_subscriber::registry().with(capture).with(hafley_observe::chrome_layer()).with(hafley_observe::SinkLayer::new(log.writer.clone()));
        let guard = subscriber.set_default();
        hafley_observe::sqlite::instrument(&db);
        let usage_before = process_sample();
        let started = Instant::now();
        {
            let _span = tracing::info_span!("crossover_mutation", state = name, arm = args[2]).entered();
            db.execute_batch("BEGIN IMMEDIATE")?;
            db.execute(state["mutation_sql"].as_str().unwrap(), [])?;
            db.execute_batch("COMMIT")?;
            db.execute_batch("CREATE TEMP TABLE measured_output AS SELECT * FROM summary;")?;
            let _: i64 = db.query_row("SELECT count(*) FROM measured_output", [], |row| row.get(0))?;
        }
        let elapsed = started.elapsed();
        let usage_after = process_sample();
        hafley_observe::sqlite::silence(&db);
        drop(guard);
        log.writer.flush();
        let actual = rows(&db, "SELECT * FROM summary")?;
        ensure!(actual == rows(&db, QUERY)?, "{name}: SQL oracle mismatch");
        let expected = state["expected"]["summary"].as_array().unwrap().iter().map(|row| row.as_array().unwrap().iter().map(|value| value.as_i64().unwrap_or_else(|| value.as_str().unwrap().parse().unwrap())).collect::<Vec<i64>>()).collect::<Vec<_>>();
        ensure!(actual == expected, "{name}: fixture oracle mismatch");
        for relation in ["dimension", "fact"] {
            ensure!(json!(rows(&db, &format!("SELECT * FROM {relation}"))?) == state["inputs"][relation], "{name}: {relation} differs");
        }
        db.execute_batch("DROP TABLE measured_output")?;
        let statements = recorder.event_sums("sqlite", tracing::Level::DEBUG, "crossover_mutation", "state", Some("sql"));
        let sql = statements.into_iter().map(|((_, sql), sums)| {
            let plan = hafley_observe::sqlite::query_plan(&db, &sql);
            json!({"query_plan":plan.as_ref().ok(),"query_plan_error":plan.as_ref().err().map(ToString::to_string),"sql":sql,"calls":sums.events,"profile_nanos":sums.sum_of("nanos"),"cumulative_vm_counter_samples_sum":sums.sum_of("vm_step")})
        }).collect::<Vec<_>>();
        let nodes = recorder.event_stats("sqlite", tracing::Level::DEBUG, "node", ["kind", "id"])
            .into_iter().map(|(key, stats)| json!({"node":key,"calls":stats.events,"profile_nanos":stats.fields.get("nanos").map(|f|f.sum())})).collect::<Vec<_>>();
        let sites = recorder.event_stats("sqlite", tracing::Level::DEBUG, "stmt", ["phase", "verb", "site"])
            .into_iter().map(|(key, stats)| json!({"site":key,"calls":stats.events,"rows_samples_sum":stats.ancestor_fields.get("rows").map(|f|f.sum()),"profile_nanos":stats.fields.get("nanos").map(|f|f.sum()),"p99_nanos":stats.fields.get("nanos").and_then(|f|f.percentile(99.0))})).collect::<Vec<_>>();
        report.push(json!({"state":name,"diagnostic_wall_ms":elapsed.as_secs_f64()*1000.0,"cpu_user_secs":usage_after.cpu_user_secs-usage_before.cpu_user_secs,"cpu_system_secs":usage_after.cpu_system_secs-usage_before.cpu_system_secs,"peak_rss_bytes":usage_after.peak_rss_bytes,"disk_write_bytes":usage_after.disk_write_bytes.zip(usage_before.disk_write_bytes).map(|(a,b)|a.saturating_sub(b)),"sql":sql,"nodes":nodes,"sites":sites}));
        hafley_observe::finish_trace();
    }
    let fixture_hash = format!("{:x}", Sha256::digest(fs::read(&args[0])?));
    let binary_hash = format!("{:x}", Sha256::digest(fs::read(std::env::current_exe()?)?));
    let database_bytes: i64 = db.query_row("SELECT page_count*page_size FROM pragma_page_count,pragma_page_size", [], |row| row.get(0))?;
    fs::write(out.join("profile.json"), serde_json::to_vec_pretty(&json!({"arm":args[2],"sqlite_version":rusqlite::version(),"fixture":args[0],"fixture_sha256":fixture_hash,"binary_sha256":binary_hash,"database_bytes":database_bytes,"counter_scope":"Raw SQLite profile counters are cumulative per statement handle. Nested profile times overlap. Neither is summed as total work. Capture, timeline and SQLite event-sink overhead are included in diagnostic wall time.","states":report}))?)?;
    Ok(())
}
