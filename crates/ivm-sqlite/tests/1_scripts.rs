#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_sqlite::Sqlite;

#[test]
fn access_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("0_access"));
}

#[test]
fn antijoin_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("4_antijoin"));
}

#[test]
fn team_sum_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("10_team_sum"));
}

#[test]
fn self_join_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("5_self_join"));
}

#[test]
fn timing_and_cells_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("8_timing_and_cells"));
}

#[test]
fn reach_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("7_reach"));
}
