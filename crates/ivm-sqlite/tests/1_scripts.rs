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
