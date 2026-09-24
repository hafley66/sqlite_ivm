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
