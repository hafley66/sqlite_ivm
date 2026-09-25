//! The named scripts of 0_scripts.rs, run through the SQLite engine against the same oracle.
#![cfg(feature = "sqlite")]

mod support;

use lab_20260924_0::Sql;

#[test]
fn s_a_access() {
    println!("{}", support::run::<Sql>("0_access"));
}

#[test]
fn s_g_team_cost() {
    println!("{}", support::run::<Sql>("1_team_cost"));
}

#[test]
fn s_w_weights_access() {
    println!("{}", support::run::<Sql>("2_weights_access"));
}

#[test]
fn s_w_weights_team_cost() {
    println!("{}", support::run::<Sql>("3_weights_team_cost"));
}

#[test]
fn s_n1_antijoin() {
    println!("{}", support::run::<Sql>("4_antijoin"));
}

#[test]
fn s_n4_self_join() {
    println!("{}", support::run::<Sql>("5_self_join"));
}

#[test]
fn s_n3_topk() {
    println!("{}", support::run::<Sql>("6_topk"));
}

#[test]
fn s_n2_reach() {
    println!("{}", support::run::<Sql>("7_reach"));
}

/// DRed case absent from the oracle: 3→1 feeds the 1⇄2 cycle, so after `-e(3,1)` row (3,1) is derivable only from itself.
#[test]
fn dred_self_supporting_cycle_matches_dd() {
    use lab_20260924_0::{Dd, Engine, Frontier, Program, SourceChange};
    let json = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/oracle/7_reach.program.json")).unwrap();
    let program: Program = serde_json::from_str(&json).unwrap();
    let (mut dd, mut sql) = (Dd::install(&program).unwrap(), Sql::install(&program).unwrap());
    let edge = |x, y, w| SourceChange { rel: 0, row: vec![x, y], w };
    let steps = [
        vec![edge(1, 2, 1), edge(2, 1, 1), edge(3, 1, 1)],
        vec![edge(3, 1, -1)],
        vec![edge(3, 1, 1), edge(1, 2, -1)],
        vec![edge(1, 2, 1), edge(2, 1, -1), edge(2, 1, 1)],
    ];
    for changes in steps {
        let frontier = Frontier { changes };
        assert_eq!(sql.settle(frontier.clone()).unwrap(), dd.settle(frontier).unwrap());
        assert_eq!(sql.snapshot(2).unwrap(), dd.snapshot(2).unwrap());
    }
}

/// VM instructions spent by one settle after a load, counted by a progress handler on the engine's connection.
fn steps_of_one_change(program: &lab_20260924_0::Program, load: Vec<lab_20260924_0::SourceChange>, change: lab_20260924_0::SourceChange) -> u64 {
    use lab_20260924_0::{Engine, Frontier};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    extern "C" fn tick(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
        unsafe { &*(counter as *const AtomicU64) }.fetch_add(1, Ordering::Relaxed);
        0
    }
    let steps = Arc::new(AtomicU64::new(0));
    let counter = Arc::as_ptr(&steps) as *mut std::ffi::c_void;
    // `steps` outlives `sql`, which is dropped at the end of this function first.
    let hook = Box::new(move |conn: &rusqlite::Connection| unsafe {
        rusqlite::ffi::sqlite3_progress_handler(conn.handle(), 1, Some(tick), counter)
    });
    let mut sql = Sql::install_observed(program, hook).unwrap();
    sql.settle(Frontier { changes: load }).unwrap();
    steps.store(0, Ordering::Relaxed);
    sql.settle(Frontier { changes: vec![change] }).unwrap();
    let n = steps.load(Ordering::Relaxed);
    drop(sql);
    n
}

/// K1 for SQLite: a constant-size change costs VM work independent of the loaded state.
#[test]
fn k1_sqlite_one_row_change_work_is_independent_of_loaded_size() {
    use lab_20260924_0::SourceChange;
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

/// Query plans of every settle statement: integrated tables (aliases end in `_i`) are only SEARCHed.
#[test]
fn k1_sqlite_no_scan_of_integrated_tables() {
    use lab_20260924_0::Engine;
    let mut scans = Vec::new();
    for name in ["0_access", "1_team_cost", "4_antijoin", "5_self_join", "6_topk", "7_reach"] {
        let sql = Sql::install(&support::program(name)).unwrap();
        for stmt in sql.statements() {
            let mut eqp = sql.conn.prepare(&format!("EXPLAIN QUERY PLAN {stmt}")).unwrap();
            let zeros = vec![0i64; eqp.parameter_count()];
            let details: Vec<String> = eqp.query_map(rusqlite::params_from_iter(zeros), |r| r.get(3)).unwrap().map(Result::unwrap).collect();
            // An empty-key Reduce joins its own output on `ON 1`; that table holds at most one row.
            let one_row = stmt.contains("x_i ON 1)");
            let scanned = |d: &&String| !one_row && d.starts_with("SCAN") && d.split_whitespace().nth(1).is_some_and(|t| t.ends_with("_i"));
            for d in details.iter().filter(scanned) {
                scans.push(format!("{name}: {d}\n    in {stmt}"));
            }
        }
    }
    assert!(scans.is_empty(), "{} scans:\n{}", scans.len(), scans.join("\n"));
}

#[test]
fn s_g_accumulable_team_sum() {
    println!("{}", support::run::<Sql>("10_team_sum"));
}

#[test]
fn s_n5_depth_cap() {
    println!("{}", support::run::<Sql>("9_depth_cap"));
}

#[test]
fn s_t_s_i_timing_and_cells() {
    println!("{}", support::run::<Sql>("8_timing_and_cells"));
}

#[test]
fn pokemon_0_can_surf() {
    println!("{}", support::run::<Sql>("pokemon/0_can_surf"));
}

#[test]
fn pokemon_1_party_stats() {
    println!("{}", support::run::<Sql>("pokemon/1_party_stats"));
}

#[test]
fn pokemon_2_party_size() {
    println!("{}", support::run::<Sql>("pokemon/2_party_size"));
}

#[test]
fn pokemon_3_rematch() {
    println!("{}", support::run::<Sql>("pokemon/3_rematch"));
}

#[test]
fn pokemon_4_two_roads() {
    println!("{}", support::run::<Sql>("pokemon/4_two_roads"));
}

#[test]
fn pokemon_5_leads() {
    println!("{}", support::run::<Sql>("pokemon/5_leads"));
}

#[test]
fn pokemon_6_walk() {
    println!("{}", support::run::<Sql>("pokemon/6_walk"));
}

#[test]
fn pokemon_7_rare_candy() {
    println!("{}", support::run::<Sql>("pokemon/7_rare_candy"));
}
