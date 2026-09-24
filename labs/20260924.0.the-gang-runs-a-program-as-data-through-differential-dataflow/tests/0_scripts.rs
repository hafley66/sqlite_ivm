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

#[test]
fn s_n1_antijoin() {
    println!("{}", support::run::<Dd>("4_antijoin"));
}

#[test]
fn s_n4_self_join() {
    println!("{}", support::run::<Dd>("5_self_join"));
}

#[test]
fn s_n3_topk() {
    println!("{}", support::run::<Dd>("6_topk"));
}

#[test]
fn s_n2_reach() {
    println!("{}", support::run::<Dd>("7_reach"));
}

#[test]
fn s_t_s_i_timing_and_cells() {
    println!("{}", support::run::<Dd>("8_timing_and_cells"));
}

#[test]
fn s_n5_depth_cap() {
    println!("{}", support::run::<Dd>("9_depth_cap"));
}
