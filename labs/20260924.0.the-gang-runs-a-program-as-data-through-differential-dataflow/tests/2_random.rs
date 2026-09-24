//! Random differential tests (plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, random harness, K4, K7).
//! Knobs: RANDOM_SEED=n runs one seed; RANDOM_CASES and RANDOM_BASE pick the seed range.

mod random;

use lab_20260924_0::Dd;
use random::{drive, meta, run};

#[test]
fn random_dd() {
    run("random_dd", 200, drive::oracle::<Dd>);
}

#[test]
fn permute_dd() {
    run("permute_dd", 100, meta::permute::<Dd>);
}

#[test]
fn split_dd() {
    run("split_dd", 100, meta::split::<Dd>);
}

#[cfg(feature = "sqlite")]
#[test]
fn random_sql() {
    run("random_sql", 200, drive::oracle::<lab_20260924_0::Sql>);
}
