//! Named scripts from plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, two programs through one API (C1).

mod support;

use lab_20260924_0::Dd;

#[test]
fn s_a_access() {
    println!("{}", support::run::<Dd>("0_access"));
}

#[test]
fn s_g_team_cost() {
    println!("{}", support::run::<Dd>("1_team_cost"));
}

#[test]
fn s_w_weights_access() {
    println!("{}", support::run::<Dd>("2_weights_access"));
}

#[test]
fn s_w_weights_team_cost() {
    println!("{}", support::run::<Dd>("3_weights_team_cost"));
}
