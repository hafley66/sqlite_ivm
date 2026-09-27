//! Generated programs against the SQLite recompute oracle and K5 value transform.

#[path = "../../ivm-dd/tests/random/mod.rs"]
mod random;

use ivm_sqlite::Sqlite;
use ivm_dd::Dd;
use random::{drive, meta, run};
use drive::Case;
use std::process::Command;

#[test]
fn captured_oracle_ir_agrees_with_dd() {
    // Lowered from sprefa oracle/eval on 2026-09-27. Each engine's bootstrap
    // frontiers carry its own intern ids; agreement decodes terms before comparing.
    for (name, source) in [
        ("3_count", include_str!("corpus/9_3_count_case.json")),
        ("16_intern_row_reuse", include_str!("corpus/10_16_intern_row_reuse_case.json")),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(source).unwrap();
        let program: ivm_ir::Program = serde_json::from_value(fixture["program"].clone()).unwrap();
        let texts: Vec<String> = serde_json::from_value(fixture["texts"].clone()).unwrap();
        let dd_frontiers: Vec<ivm_ir::Frontier> = serde_json::from_value(fixture["dd_frontiers"].clone()).unwrap();
        let sqlite_frontiers: Vec<ivm_ir::Frontier> = serde_json::from_value(fixture["sqlite_frontiers"].clone()).unwrap();
        drive::agreement_frontiers::<Dd, Sqlite>(&program, &texts, &dd_frontiers, &sqlite_frontiers)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

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
fn random_typed_dd_sql() {
    isolated("random_typed_dd_sql_worker");
}

#[test]
#[ignore = "run by random_typed_dd_sql in a separate process"]
fn random_typed_dd_sql_worker() {
    run("random_typed_dd_sql", 1000, Case::generate_typed, drive::agreement::<Dd, Sqlite>);
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
