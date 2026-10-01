//! SQLite VM work after a load, using the same K1 sizes as the lab gate.

#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_dd::{Engine, Frontier, Program, Raw, SourceChange};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};

extern "C" fn tick(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe { &*(counter as *const AtomicU64) }.fetch_add(1, Ordering::Relaxed);
    0
}

fn steps_of_one_change(program: &Program, load: Vec<SourceChange>, change: SourceChange) -> u64 {
    let db = Connection::open_in_memory().unwrap();
    let mut sql = Sqlite::install(program, &mut Raw::with_connection(&db)).unwrap();
    sql.settle(Frontier { changes: load }, &mut Raw::with_connection(&db)).unwrap();
    let steps = Arc::new(AtomicU64::new(0));
    let counter = Arc::as_ptr(&steps) as *mut std::ffi::c_void;
    unsafe { rusqlite::ffi::sqlite3_progress_handler(db.handle(), 1, Some(tick), counter); }
    sql.settle(Frontier { changes: vec![change] }, &mut Raw::with_connection(&db)).unwrap();
    unsafe { rusqlite::ffi::sqlite3_progress_handler(db.handle(), 0, None, std::ptr::null_mut()); }
    steps.load(Ordering::Relaxed)
}

#[test]
fn k1_sqlite_one_row_change_work_is_independent_of_loaded_size() {
    let row = |rel, row: Vec<i64>| SourceChange { rel, row, w: 1 };
    let access = support::program("0_access");
    let access_at = |n: i64| {
        let mut load = vec![row(1, vec![10, 100])];
        load.extend((0..n).map(|person| row(0, vec![person, 10])));
        steps_of_one_change(&access, load, row(2, vec![-1, 7]))
    };
    let (small, large) = (access_at(1_000), access_at(30_000));
    println!("access: {small} VM steps at 1e3, {large} at 3e4");
    assert!(large <= small * 2, "access: {small} VM steps at 1e3, {large} at 3e4");

    let team_cost = support::program("1_team_cost");
    let team_cost_at = |n: i64| {
        let mut load: Vec<SourceChange> = (0..n).map(|id| row(0, vec![id, 1000 + id % 100, id])).collect();
        load.extend((0..5).map(|k| row(0, vec![-1 - k, 10, k])));
        steps_of_one_change(&team_cost, load, row(0, vec![-100, 10, 7]))
    };
    let (small, large) = (team_cost_at(1_000), team_cost_at(30_000));
    println!("team_cost: {small} VM steps at 1e3, {large} at 3e4");
    assert!(large <= small * 2, "team_cost: {small} VM steps at 1e3, {large} at 3e4");
}

#[test]
fn k1_sqlite_no_scan_of_integrated_tables() {
    let mut scans = Vec::new();
    for name in ["0_access", "1_team_cost", "4_antijoin", "5_self_join", "6_topk", "7_reach"] {
        let db = Connection::open_in_memory().unwrap();
        let sql = Sqlite::install(&support::program(name), &mut Raw::with_connection(&db)).unwrap();
        for stmt in sql.statements() {
            let mut eqp = db.prepare(&format!("EXPLAIN QUERY PLAN {stmt}")).unwrap();
            let zeros = vec![0i64; eqp.parameter_count()];
            let details: Vec<String> = eqp.query_map(rusqlite::params_from_iter(zeros), |r| r.get(3)).unwrap().map(Result::unwrap).collect();
            let one_row = stmt.contains("x_i ON 1)");
            for detail in details.iter().filter(|detail| {
                !one_row && detail.starts_with("SCAN") && detail.split_whitespace().nth(1).is_some_and(|table| table.ends_with("_i"))
            }) {
                scans.push(format!("{name}: {detail}\n    in {stmt}"));
            }
        }
    }
    assert!(scans.is_empty(), "{} scans:\n{}", scans.len(), scans.join("\n"));
}

/// Hot shape from the dl8 compiler: two `Ty::Id` relations joined on an Id column, then an Id
/// equality filter. Ids are hash-consed, so the join and the filter are integer `=` that probe the
/// integrated key index; no dictionary function appears in any predicate.
#[test]
fn id_join_probes_the_key_index_without_dictionary_functions() {
    let program: Program = serde_json::from_str(r#"{
        "rels": [
            {"id": 0, "name": "edge", "cols": ["Id", "Id"], "kind": "Source"},
            {"id": 1, "name": "label", "cols": ["Id", "Text"], "kind": "Source"},
            {"id": 2, "name": "out", "cols": ["Id", "Id", "Text"], "kind": "Derived"}
        ],
        "nodes": [
            {"Get": 0},
            {"Get": 1},
            {"Join": {"inputs": [0, 1], "equivalences": [[[0, 1], [1, 0]]]}},
            {"Mfp": {"input": 2, "filter": [{"Call": ["Ne", [{"Col": 0}, {"Col": 1}]]}], "project": [0, 1, 3]}}
        ],
        "strata": [{"Let": {"id": 2, "body": 3}}],
        "outputs": [2]
    }"#).unwrap();
    let db = Connection::open_in_memory().unwrap();
    let sql = Sqlite::install(&program, &mut Raw::with_connection(&db)).unwrap();
    let statements = sql.statements();
    let udf = statements.iter().filter(|s| ["ivm_term_key(", "ivm_text_value(", "ivm_any_value("].iter().any(|f| s.contains(f))).count();
    let join = statements.iter().find(|s| s.contains("CROSS JOIN") && s.contains("l_i") && s.contains("r_d")).unwrap();
    let mut eqp = db.prepare(&format!("EXPLAIN QUERY PLAN {join}")).unwrap();
    let plan: Vec<String> = eqp.query_map([], |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap)
        .filter(|detail| detail.contains("_i "))
        .collect();
    assert_eq!((udf, plan), (0, vec![
        "SEARCH l_i USING INDEX frontier_out_n0_x1 (c1=?)".to_owned(),
        "SEARCH r_i USING PRIMARY KEY (c0=?)".to_owned(),
    ]));
}
