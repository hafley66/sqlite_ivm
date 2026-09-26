//! Measurement example: the numbers recorded in HYPOTHESIS.md.
//!
//! Runs both packet cases against a file-backed database under
//! `hafley-observe` instrumentation, then prints PROFILE aggregates, prepare
//! counts by cache disposition, engine SQL-byte totals, object/index counts,
//! allocator memory, process peak RSS, and database file sizes.
//!
//! Run: `cargo run --offline --release --example measure`

use ivm_sqlite::{Cell, Frontier, Program};
use hafley_observe::rusage;
use hafley_observe::sqlite_memory;
use hafley_observe::CountRecorder;
use rusqlite::Connection;
use std::path::PathBuf;
use tracing::Level;
use tracing_subscriber::prelude::*;

fn file_db() -> Connection {
    let path: PathBuf = std::env::temp_dir().join("frontier_measure.db");
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
         CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
         CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
         CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);",
    )
    .unwrap();
    conn
}

fn frontier(conn: &Connection, sql: &str) {
    conn.execute_batch(&format!("BEGIN; {sql} COMMIT;"))
        .unwrap();
}

fn main() {
    let (recorder, layer) = CountRecorder::new();
    let _guard = tracing_subscriber::registry().with(layer).set_default();

    let conn = file_db();
    hafley_observe::sqlite::instrument(&conn);

    let access = Program::install(
        &conn,
        "access",
        "SELECT person, resource FROM direct_grant \
         UNION \
         SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team",
    )
    .unwrap();
    let team_cost = Program::install(
        &conn,
        "team_cost",
        "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team",
    )
    .unwrap();

    // The 7 access frontiers of the packet.
    let access_frontiers = [
        "INSERT INTO membership VALUES (1,10),(1,20); INSERT INTO permission VALUES (10,100),(20,100); INSERT INTO direct_grant VALUES (3,300);",
        "INSERT INTO membership VALUES (2,10); INSERT INTO permission VALUES (10,200);",
        "INSERT INTO direct_grant VALUES (1,200);",
        "DELETE FROM membership WHERE person=1 AND team=10;",
        "DELETE FROM permission WHERE team=20 AND resource=100;",
        "DELETE FROM direct_grant WHERE person=1 AND resource=200;",
        "UPDATE permission SET resource=300 WHERE team=10 AND resource=200;",
    ];
    for f in access_frontiers {
        frontier(&conn, f);
    }
    // Three repeated same-shape frontiers. Every settle re-runs its fixed
    // statement set through the prepare cache (rusqlite's prepare_cached
    // reuses compiled statements), so the guardrail is: equal VM steps across
    // warm repeats (deterministic cost) and zero fresh prepares added.
    frontier(&conn, "INSERT INTO direct_grant VALUES (9,900);");
    let cum_1 = settle_vm_steps(&recorder);
    let (fresh_1, cached_1) = prepare_counts(&recorder);
    frontier(&conn, "INSERT INTO direct_grant VALUES (9,901);");
    let cum_2 = settle_vm_steps(&recorder);
    let (_fresh_2, cached_2) = prepare_counts(&recorder);
    frontier(&conn, "INSERT INTO direct_grant VALUES (9,902);");
    let cum_3 = settle_vm_steps(&recorder);
    let (fresh_3, _cached_3) = prepare_counts(&recorder);
    let steps_a = cum_2 - cum_1;
    let steps_b = cum_3 - cum_2;
    let fresh_added = fresh_3 - fresh_1;
    let cached_per_settle = cached_2 - cached_1;

    // The aggregate frontiers.
    for f in [
        "INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11);",
        "INSERT INTO job VALUES (4,10,3); UPDATE job SET team=10 WHERE id=3;",
        "UPDATE job SET cost=-7 WHERE id=2;",
        "DELETE FROM job WHERE id IN (1,4);",
        "DELETE FROM job WHERE id IN (2,3);",
    ] {
        frontier(&conn, f);
    }

    println!("== frontier engine measurements ==");
    println!(
        "access frontier id      : {}",
        access.frontier_id(&conn).unwrap()
    );
    println!(
        "team_cost frontier id   : {}",
        team_cost.frontier_id(&conn).unwrap()
    );

    // Prepare counts by cache disposition: `stmt` spans record whether the
    // statement went through the fresh prepare path or the cache.
    let prepared = recorder.span_counts_by_field("stmt", "prepared");
    println!("\n[prepares]");
    for (kind, count) in &prepared {
        println!("  {kind}: {count}");
    }
    println!(
        "  (cached/fresh spans count prepare calls; every settle re-runs a fixed statement set)"
    );

    // PROFILE: statement events under `stmt`, grouped by cache disposition.
    println!("\n[sqlite profile: vm steps by statement cache]");
    let groups = recorder.event_sums("sqlite", Level::DEBUG, "stmt", "prepared", None);
    let mut total_steps = 0.0;
    let mut total_statements = 0.0;
    for ((disposition, _sql), sums) in &groups {
        let steps = sums.sum_of("vm_step");
        total_steps += steps;
        total_statements += sums.events as f64;
        // The empty disposition covers statements run outside the engine's
        // cached prepare helper (install-time DDL batches).
        let label = if disposition.is_empty() {
            "other (DDL batches)"
        } else {
            disposition
        };
        println!(
            "  {label}: {} statements, {} vm steps",
            sums.events, steps as u64
        );
    }
    println!(
        "  total: {} statements, {} vm steps",
        total_statements as u64, total_steps as u64
    );

    // Engine settle events: settled frontiers, statements, and SQL bytes per program.
    let settle_sums =
        recorder.event_sums("frontier", Level::DEBUG, "frontier_settle", "program", None);
    println!("\n[engine settles]");
    for ((program, _), sums) in &settle_sums {
        println!(
            "  {program}: {} frontiers, {} statements, {} sql bytes",
            sums.events,
            sums.sum_of("sql_statements") as u64,
            sums.sum_of("sql_bytes") as u64
        );
    }

    // Object and index inventory.
    println!("\n[engine objects in sqlite_master]");
    let mut stmt = conn
        .prepare("SELECT type, count(*) FROM sqlite_master WHERE name LIKE 'frontier\\_%' ESCAPE '\\' GROUP BY type ORDER BY type")
        .unwrap();
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (kind, count) in rows {
        println!("  {kind}: {count}");
    }
    let total: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'frontier\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("  total: {total}");

    // Memory.
    println!("\n[memory]");
    if let Some(mem) = sqlite_memory::sample(&conn) {
        println!("  allocator current: {} bytes", mem.allocator_current_bytes);
        println!("  allocator peak   : {} bytes", mem.allocator_peak_bytes);
        println!(
            "  connection stmts : {} bytes",
            mem.connection_statement_bytes
        );
        println!("  connection cache : {} bytes", mem.connection_cache_bytes);
        println!("  connection schema: {} bytes", mem.connection_schema_bytes);
    } else {
        println!("  sqlite memory status unavailable");
    }

    println!("\n[process]");
    let usage = rusage::sample();
    if let Some(rss) = usage.peak_rss_bytes {
        println!("  peak rss: {} bytes", rss);
    }
    println!(
        "  cpu user: {:.4}s, system: {:.4}s",
        usage.cpu_user_secs, usage.cpu_system_secs
    );

    println!("\n[database files]");
    let db_path = std::env::temp_dir().join("frontier_measure.db");
    for suffix in ["", "-wal"] {
        let mut p = db_path.clone().into_os_string();
        p.push(suffix);
        if let Ok(meta) = std::fs::metadata(&p) {
            println!("  {} : {} bytes", p.display(), meta.len());
        }
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let mut p = db_path.into_os_string();
    p.push("-wal");
    match std::fs::metadata(&p) {
        Ok(meta) => println!("  wal after checkpoint(TRUNCATE): {} bytes", meta.len()),
        Err(_) => println!("  wal after checkpoint(TRUNCATE): absent"),
    }

    // Snapshot sanity: the engine's visible output is what the oracles predict.
    let visible: Vec<(i64, i64, i64)> = team_cost
        .snapshot(&conn)
        .unwrap()
        .into_iter()
        .map(|t| match (&t.0[0], &t.0[1], &t.0[2]) {
            (Cell::Integer(a), Cell::Integer(b), Cell::Integer(c)) => (*a, *b, *c),
            _ => panic!(),
        })
        .collect();
    println!("\nteam_cost visible rows (expected empty: all jobs deleted): {visible:?}");
    println!("\nrepeat-frontier guardrail (same-shape settles):");
    println!(
        "  vm steps repeat A={} B={} (equal means deterministic)",
        steps_a as u64, steps_b as u64
    );
    println!("  cached prepare calls in one settle: {cached_per_settle}");
    println!("  fresh prepares added across repeats: {fresh_added}");
}

/// Total vm steps across statements whose `stmt` span says CACHED, captured
/// incrementally: the recorder holds cumulative counts, so the second reading
/// minus stored history is what we compare; here we keep it simple and report
/// the cumulative delta between the two repeated frontiers via a second pass.
fn settle_vm_steps(recorder: &CountRecorder) -> f64 {
    let groups = recorder.event_sums("sqlite", Level::DEBUG, "stmt", "prepared", None);
    let mut total = 0.0;
    for ((disposition, _), sums) in groups {
        if disposition == "cached" {
            total += sums.sum_of("vm_step");
        }
    }
    total
}

fn prepare_counts(recorder: &CountRecorder) -> (usize, usize) {
    let counts = recorder.span_counts_by_field("stmt", "prepared");
    (
        counts.get("fresh").copied().unwrap_or(0),
        counts.get("cached").copied().unwrap_or(0),
    )
}
