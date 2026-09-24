//! Named scripts from plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, two programs through one API (C1).

mod support;

#[test]
fn s_a_access() {
    println!("{}", support::run("0_access"));
}

#[test]
fn s_g_team_cost() {
    println!("{}", support::run("1_team_cost"));
}
