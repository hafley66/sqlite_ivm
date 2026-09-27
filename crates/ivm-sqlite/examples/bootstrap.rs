//! Bootstrap install measurement: sources pre-populated with 32 and 1024
//! rows per table, then one `Program::install` per program shape over the
//! seeded tables. Prints install wall time, Rust peak RSS (rusage), the
//! SQLite allocator status, and the DB/WAL file bytes — the existing
//! hafley-observe counters, no bespoke telemetry.
//!
//! Peak RSS is the process maximum, so the 1024-row run's reading bounds the
//! 32-row run's; per-install RSS growth is not separable with this counter.
//!
//! Run: `cargo run --offline --release --example bootstrap`

use ivm_sqlite::{Frontier, Program};
use hafley_observe::rusage;
use hafley_observe::sqlite_memory;
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Instant;

const ACCESS_SQL: &str = "SELECT person, resource FROM direct_grant \
     UNION \
     SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team";
const TEAM_COST_SQL: &str =
    "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team";

/// A fresh file-backed database with `rows` rows pre-populated in every
/// source table, WAL mode like the measure example.
fn seeded_db(rows: usize) -> Connection {
    let path: PathBuf = std::env::temp_dir().join(format!("frontier_bootstrap_{rows}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(&p);
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
    conn.execute_batch("BEGIN;").unwrap();
    {
        let mut member = conn
            .prepare("INSERT INTO membership VALUES (?1, ?1 % 16)")
            .unwrap();
        let mut grant = conn
            .prepare("INSERT INTO direct_grant VALUES (?1, 1000 + ?1 % 16)")
            .unwrap();
        for i in 0..rows as i64 {
            member.execute([i]).unwrap();
            grant.execute([i]).unwrap();
        }
    }
    conn.execute_batch(
        "INSERT INTO permission VALUES (0, 1000), (1, 1001), (2, 1002), (3, 1003),
         (4, 1004), (5, 1005), (6, 1006), (7, 1007),
         (8, 1008), (9, 1009), (10, 1010), (11, 1011),
         (12, 1012), (13, 1013), (14, 1014), (15, 1015);
         COMMIT;",
    )
    .unwrap();
    {
        let mut job = conn
            .prepare("INSERT INTO job VALUES (?1, ?1 % 16, ?1 % 97)")
            .unwrap();
        conn.execute_batch("BEGIN;").unwrap();
        for i in 0..rows as i64 {
            job.execute([i]).unwrap();
        }
        conn.execute_batch("COMMIT;").unwrap();
    }
    conn
}

fn file_bytes(path: &PathBuf, suffix: &str) -> u64 {
    let mut p = path.clone().into_os_string();
    p.push(suffix);
    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
}

fn main() {
    println!("== frontier bootstrap install measurement ==");
    for rows in [32usize, 1024] {
        let conn = seeded_db(rows);
        hafley_observe::sqlite::instrument(&conn);
        let path: PathBuf = std::env::temp_dir().join(format!("frontier_bootstrap_{rows}.db"));

        let t = Instant::now();
        let access = Program::install(&conn, "access", ACCESS_SQL).unwrap();
        let access_us = t.elapsed().as_micros();

        let t = Instant::now();
        let team_cost = Program::install(&conn, "team_cost", TEAM_COST_SQL).unwrap();
        let group_us = t.elapsed().as_micros();

        let usage = rusage::sample();
        let mem = sqlite_memory::sample(&conn);
        println!("\n[rows per source table: {rows}]");
        println!("  access install    : {access_us} us");
        println!("  team_cost install : {group_us} us");
        println!(
            "  frontier ids      : access {}, team_cost {} (bootstrap bumps nothing)",
            access.frontier_id(&conn).unwrap(),
            team_cost.frontier_id(&conn).unwrap()
        );
        if let Some(mem) = mem {
            println!(
                "  sqlite allocator  : current {} B, peak {} B",
                mem.allocator_current_bytes, mem.allocator_peak_bytes
            );
            println!("  connection schema : {} B", mem.connection_schema_bytes);
        } else {
            println!("  sqlite allocator  : status unavailable");
        }
        match usage.peak_rss_bytes {
            Some(rss) => println!("  rust peak rss     : {rss} B (process maximum)"),
            None => println!("  rust peak rss     : rusage unavailable"),
        }
        println!("  db bytes          : {}", file_bytes(&path, ""));
        println!("  wal bytes         : {}", file_bytes(&path, "-wal"));

        access.teardown(&conn).unwrap();
        team_cost.teardown(&conn).unwrap();
        conn.close().unwrap();
    }
}
