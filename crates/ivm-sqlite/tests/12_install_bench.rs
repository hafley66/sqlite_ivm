//! Install benchmark (lab-20261005-sqlite-install). Ignored; run with
//! `cargo test --release -p ivm-sqlite --test 12_install_bench -- --ignored --nocapture --test-threads=1`.
//!
//! Corpora: the c15-shaped IR and the three IRs one `dl8 build std/tsi_registry_emit.dl7` installs
//! (macro library, macrotime, comptime; dumped with `DL8_IR_DUMP`). Per corpus and run, interleaved:
//! sqlite install ms, CREATE count, statements run by the install, CREATE execution time by schema
//! size (SQLite's `SQLITE_TRACE_PROFILE`, which excludes prepare), dd install ms, and the cold
//! prepare of every settle statement the install leaves behind.
//!
//! Knobs: `IVM_BENCH_RUNS` (default 5), `IVM_BENCH_PRAGMAS` (SQL run on the connection before the
//! install), `IVM_BENCH_FILE=1` (a file database in `TMPDIR` instead of `:memory:`).

use std::{ffi::{c_int, c_void, CStr}, time::Instant};

use ivm_dd::Dd;
use ivm_engine::Engine;
use ivm_ir::{Frontier, Program};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;

#[derive(Default)]
struct Trace {
    statements: usize,
    creates: usize,
    /// Execution ns of each CREATE, in order.
    create_ns: Vec<u64>,
    other_ns: u64,
}

extern "C" fn trace(event: u32, context: *mut c_void, statement: *mut c_void, extra: *mut c_void) -> c_int {
    let trace = unsafe { &mut *(context as *mut Trace) };
    let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement.cast()) };
    let create = !sql.is_null() && unsafe { CStr::from_ptr(sql) }.to_bytes().starts_with(b"CREATE");
    if event == rusqlite::ffi::SQLITE_TRACE_STMT as u32 {
        trace.statements += 1;
        trace.creates += create as usize;
    } else if event == rusqlite::ffi::SQLITE_TRACE_PROFILE as u32 {
        let ns = unsafe { *(extra as *const i64) } as u64;
        if create { trace.create_ns.push(ns) } else { trace.other_ns += ns }
    }
    0
}

fn connection() -> Connection {
    let db = if std::env::var_os("IVM_BENCH_FILE").is_some() {
        let path = std::env::temp_dir().join(format!("ivm_install_bench_{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        Connection::open(path).unwrap()
    } else {
        Connection::open_in_memory().unwrap()
    };
    if let Ok(pragmas) = std::env::var("IVM_BENCH_PRAGMAS") {
        db.execute_batch(&pragmas).unwrap();
    }
    db
}

struct Run {
    install_ms: f64,
    trace: Trace,
    settle_statements: usize,
    prepare_ms: f64,
    dd_ms: f64,
}

fn run(program: &Program) -> Run {
    let db = connection();
    let mut traced = Box::new(Trace::default());
    let handle = unsafe { db.handle() };
    let mask = (rusqlite::ffi::SQLITE_TRACE_STMT | rusqlite::ffi::SQLITE_TRACE_PROFILE) as u32;
    assert_eq!(unsafe { rusqlite::ffi::sqlite3_trace_v2(handle, mask, Some(trace),
        (&mut *traced as *mut Trace).cast()) }, rusqlite::ffi::SQLITE_OK);
    let start = Instant::now();
    let mut engine = Sqlite::install_on(db, program).unwrap();
    let install_ms = start.elapsed().as_secs_f64() * 1e3;
    unsafe { rusqlite::ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut()); }
    let statements = engine.statements().into_iter().map(str::to_owned).collect::<Vec<_>>();
    let start = Instant::now();
    for sql in &statements {
        // Fresh prepares: what the first settle pays through its statement cache.
        let _ = engine.db.prepare(sql);
    }
    let prepare_ms = start.elapsed().as_secs_f64() * 1e3;
    engine.settle(Frontier { changes: vec![] }).unwrap();
    let start = Instant::now();
    let dd = <Dd as Engine>::install(program).unwrap();
    let dd_ms = start.elapsed().as_secs_f64() * 1e3;
    drop(dd);
    Run { install_ms, trace: *traced, settle_statements: statements.len(), prepare_ms, dd_ms }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

#[test]
#[ignore]
fn install_bench() {
    let corpora: [(&str, &str); 4] = [
        ("c15", include_str!("corpus/8_c15_program.json")),
        ("macro_library", include_str!("corpus/install/1_macro_library.json")),
        ("macrotime", include_str!("corpus/install/2_macrotime.json")),
        ("registry_comptime", include_str!("corpus/install/3_registry_comptime.json")),
    ];
    let programs = corpora.map(|(name, text)| (name, serde_json::from_str::<Program>(text).unwrap()));
    let runs: usize = std::env::var("IVM_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let mut results: Vec<Vec<Run>> = programs.iter().map(|_| Vec::new()).collect();
    // Warm-up pass, then interleaved runs.
    for (_, program) in &programs { run(program); }
    for _ in 0..runs {
        for (at, (_, program)) in programs.iter().enumerate() { results[at].push(run(program)); }
    }
    for ((name, program), runs) in programs.iter().zip(results) {
        let last = runs.last().unwrap();
        let creates = last.trace.create_ns.len();
        let bucket = (creates / 5).max(1);
        let buckets = last.trace.create_ns.chunks(bucket)
            .map(|chunk| format!("{:.1}", chunk.iter().sum::<u64>() as f64 / chunk.len() as f64 / 1e3))
            .collect::<Vec<_>>().join("/");
        let create_ms = median(runs.iter().map(|r| r.trace.create_ns.iter().sum::<u64>() as f64 / 1e6).collect());
        let other_ms = median(runs.iter().map(|r| r.trace.other_ns as f64 / 1e6).collect());
        let install = runs.iter().map(|r| format!("{:.1}", r.install_ms)).collect::<Vec<_>>().join(",");
        eprintln!(
            "install_bench corpus={name} nodes={} install_ms_median={:.1} install_ms=[{install}] creates={} install_statements={} \
             create_exec_ms={create_ms:.1} other_exec_ms={other_ms:.1} create_us_by_fifth={buckets} \
             settle_statements={} cold_prepare_ms={:.1} dd_install_ms_median={:.1}",
            program.nodes.len(),
            median(runs.iter().map(|r| r.install_ms).collect()),
            last.trace.creates,
            last.trace.statements,
            last.settle_statements,
            median(runs.iter().map(|r| r.prepare_ms).collect()),
            median(runs.iter().map(|r| r.dd_ms).collect()),
        );
    }
}

/// One synthetic schema of `n` objects shaped like an install (a `_d` heap table, a keyed
/// `WITHOUT ROWID` `_i` and one index per node, 3 objects per node), with `shards` attached
/// in-memory schemas taking the objects round-robin. Returns the batch text.
fn synthetic_ddl(n: usize, shards: usize) -> String {
    let mut sql = String::from("SAVEPOINT s;");
    for k in 0..n / 3 {
        let schema = if shards == 0 { "main".to_owned() } else { format!("s{}", k % shards) };
        sql.push_str(&format!(
            "CREATE TABLE {schema}.ivm_n{k}_d (c0 INTEGER NOT NULL, c1 INTEGER NOT NULL, c2 INTEGER NOT NULL, w INTEGER NOT NULL);\
             CREATE TABLE {schema}.ivm_n{k}_i (c0 INTEGER NOT NULL, c1 INTEGER NOT NULL, c2 INTEGER NOT NULL, w INTEGER NOT NULL, PRIMARY KEY (c0, c1, c2)) WITHOUT ROWID;\
             CREATE INDEX {schema}.ivm_n{k}_x0 ON ivm_n{k}_i (c1);"
        ));
    }
    sql.push_str("RELEASE s;");
    sql
}

fn exec_c(db: &Connection, sql: &str) {
    let text = std::ffi::CString::new(sql).unwrap();
    let rc = unsafe {
        rusqlite::ffi::sqlite3_exec(db.handle(), text.as_ptr(), None, std::ptr::null_mut(), std::ptr::null_mut())
    };
    assert_eq!(rc, rusqlite::ffi::SQLITE_OK, "{}", db.last_insert_rowid());
}

/// A1: DDL cost per object against schema size, and what changes it. Ignored; run with
/// `cargo test --release -p ivm-sqlite --test 12_install_bench create_scaling -- --ignored --nocapture`.
#[test]
#[ignore]
fn create_scaling() {
    let runs: usize = std::env::var("IVM_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let variants: [(&str, usize, bool, &str); 7] = [
        ("execute_batch", 0, false, ""),
        ("sqlite3_exec", 0, true, ""),
        ("exec+pragmas", 0, true, "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA foreign_keys=OFF;"),
        ("exec+shards2", 2, true, ""),
        ("exec+shards4", 4, true, ""),
        ("exec+shards8", 8, true, ""),
        ("exec+shards10", 10, true, ""),
    ];
    for n in [750, 1500, 3000, 6000] {
        for (name, shards, raw, pragmas) in variants {
            let sql = synthetic_ddl(n, shards);
            let mut ms = Vec::new();
            for _ in 0..runs {
                let db = Connection::open_in_memory().unwrap();
                db.execute_batch(pragmas).unwrap();
                for s in 0..shards { db.execute_batch(&format!("ATTACH ':memory:' AS s{s}")).unwrap(); }
                let start = Instant::now();
                if raw { exec_c(&db, &sql) } else { db.execute_batch(&sql).unwrap() }
                ms.push(start.elapsed().as_secs_f64() * 1e3);
            }
            let median = median(ms);
            eprintln!("create_scaling objects={n} variant={name} ms_median={median:.1} us_per_object={:.1}", median * 1e3 / n as f64);
        }
    }
}
