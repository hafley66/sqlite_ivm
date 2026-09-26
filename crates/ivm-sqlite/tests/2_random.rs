//! Generated programs against the SQLite recompute oracle and K5 value transform.

#[path = "../../ivm-dd/tests/random/mod.rs"]
mod random;

use ivm_sqlite::Sqlite;
use random::{drive, meta, run};
use drive::Case;

#[test]
fn random_sql() {
    run("random_sql", 200, Case::generate, drive::oracle::<Sqlite>);
}

#[test]
fn k5_sql() {
    run("k5_sql", 100, Case::generate_k5, meta::values::<Sqlite>);
}

#[test]
fn k5_oracle_sql() {
    run("k5_oracle_sql", 100, Case::generate_k5, drive::oracle::<Sqlite>);
}
