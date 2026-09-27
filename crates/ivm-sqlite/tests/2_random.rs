//! Generated programs against the SQLite recompute oracle and K5 value transform.

#[path = "../../ivm-dd/tests/random/mod.rs"]
mod random;

use ivm_sqlite::Sqlite;
use ivm_dd::Dd;
use random::{drive, meta, run};
use drive::Case;
use std::process::Command;

fn isolated(worker: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", worker, "--nocapture"])
        .status()
        .unwrap();
    assert!(status.success(), "{worker} exited with {status}");
}

#[test]
fn random_sql() {
    isolated("random_sql_worker");
}

#[test]
#[ignore = "run by random_sql in a separate process"]
fn random_sql_worker() {
    run("random_sql", 200, Case::generate, drive::oracle::<Sqlite>);
}

#[test]
fn random_mint_dd_sql() {
    isolated("random_mint_dd_sql_worker");
}

#[test]
#[ignore = "run by random_mint_dd_sql in a separate process"]
fn random_mint_dd_sql_worker() {
    for seed in drive::seeds(200) {
        let case = Case::generate_mint(seed);
        drive::agreement::<Dd, Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        drive::term_lt_structure::<Dd>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        drive::term_lt_structure::<Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn random_recursive_shapes_dd_sql() {
    isolated("random_recursive_shapes_dd_sql_worker");
}

#[test]
#[ignore = "run by random_recursive_shapes_dd_sql in a separate process"]
fn random_recursive_shapes_dd_sql_worker() {
    for seed in drive::seeds(200) {
        let case = Case::generate_recursive_shapes(seed);
        drive::agreement::<Dd, Sqlite>(&case).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
    }
}

#[test]
fn k5_sql() {
    isolated("k5_sql_worker");
}

#[test]
#[ignore = "run by k5_sql in a separate process"]
fn k5_sql_worker() {
    run("k5_sql", 100, Case::generate_k5, meta::values::<Sqlite>);
}

#[test]
fn k5_oracle_sql() {
    isolated("k5_oracle_sql_worker");
}

#[test]
#[ignore = "run by k5_oracle_sql in a separate process"]
fn k5_oracle_sql_worker() {
    run("k5_oracle_sql", 100, Case::generate_k5, drive::oracle::<Sqlite>);
}
