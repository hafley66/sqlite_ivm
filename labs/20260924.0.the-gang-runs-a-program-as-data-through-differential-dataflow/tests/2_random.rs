//! Random differential tests (plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, random harness, K4/K5/K7).
//! Knobs: RANDOM_SEED=n runs one seed; SEEDS (or RANDOM_CASES) and RANDOM_BASE pick the seed range.

mod random;

use lab_20260924_0::Dd;
use lab_20260924_0::{Op, Stratum};
use random::{drive, meta, run};
use drive::Case;

#[test]
fn generator_coverage() {
    let mut one_rec = 0;
    let mut two_rec = 0;
    let mut topk = 0;
    for seed in 0..1000 {
        let case = drive::Case::generate(seed);
        for stratum in &case.program.strata {
            if let Stratum::LetRec(rec) = stratum {
                if rec.ids.len() == 1 { one_rec += 1; } else { two_rec += 1; }
            }
        }
        if case.program.nodes.iter().any(|op| matches!(op, Op::TopK { .. })) { topk += 1; }
    }
    println!("seeds=1000 one_rec={one_rec} two_rec={two_rec} topk={topk}");
    assert!(one_rec > 0 && two_rec > 0 && topk > 0);
}

#[test]
fn random_dd() {
    run("random_dd", 200, Case::generate, drive::oracle::<Dd>);
}

#[test]
fn permute_dd() {
    run("permute_dd", 100, Case::generate, meta::permute::<Dd>);
}

#[test]
fn split_dd() {
    run("split_dd", 100, Case::generate, meta::split::<Dd>);
}

#[test]
fn k5_dd() {
    run("k5_dd", 100, Case::generate_k5, meta::values::<Dd>);
}

#[test]
fn k5_oracle_dd() {
    run("k5_oracle_dd", 100, Case::generate_k5, drive::oracle::<Dd>);
}

#[cfg(feature = "sqlite")]
#[test]
fn random_sql() {
    run("random_sql", 200, Case::generate, drive::oracle::<lab_20260924_0::Sql>);
}

#[cfg(feature = "sqlite")]
#[test]
fn k5_sql() {
    run("k5_sql", 100, Case::generate_k5, meta::values::<lab_20260924_0::Sql>);
}

#[cfg(feature = "sqlite")]
#[test]
fn k5_oracle_sql() {
    run("k5_oracle_sql", 100, Case::generate_k5, drive::oracle::<lab_20260924_0::Sql>);
}
