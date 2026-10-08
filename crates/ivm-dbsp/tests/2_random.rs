//! The ivm-dd random differential harness on the dbsp engine: SQLite recompute oracle and
//! metamorphic checks. Knobs: RANDOM_SEED=n runs one seed; SEEDS and RANDOM_BASE pick the range.

extern crate ivm_dbsp as ivm_dd;

#[path = "../../ivm-dd/tests/random/mod.rs"]
mod random;

use ivm_dbsp::Dbsp;
use random::{drive, meta, run};
use drive::Case;

#[test]
fn random_programs_match_sqlite() {
    run("random_dbsp", 200, Case::generate, drive::oracle::<Dbsp>);
}

#[test]
fn permuted_frontiers_agree() {
    run("permute_dbsp", 100, Case::generate, meta::permute::<Dbsp>);
}

#[test]
fn split_frontiers_agree() {
    run("split_dbsp", 100, Case::generate, meta::split::<Dbsp>);
}

#[test]
fn k5_values() {
    run("k5_dbsp", 100, Case::generate_k5, meta::values::<Dbsp>);
}

#[test]
fn k5_oracle() {
    run("k5_oracle_dbsp", 100, Case::generate_k5, drive::oracle::<Dbsp>);
}
