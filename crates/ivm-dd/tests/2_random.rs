//! Random differential tests (plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, random harness, K4/K5/K7).
//! Knobs: RANDOM_SEED=n runs one seed; SEEDS (or RANDOM_CASES) and RANDOM_BASE pick the seed range.

mod random;

use ivm_dd::Dd;
use ivm_dd::{Op, Stratum};
use random::{drive, meta, run};
use drive::Case;

#[test]
fn generator_coverage() {
    let mut one_rec = 0;
    let mut two_rec = 0;
    let mut topk = 0;
    let mut mint = 0;
    let mut window = 0;
    let mut window_join = 0;
    let mut window_reduce = 0;
    let mut recursive_mint = 0;
    let mut recursive_antijoin = 0;
    let mut mint_before_antijoin = 0;
    let mut antijoin_before_mint = 0;
    let mut empty_antijoin_key = 0;
    for seed in 0..1000 {
        let case = drive::Case::generate(seed);
        for stratum in &case.program.strata {
            if let Stratum::LetRec(rec) = stratum {
                if rec.ids.len() == 1 { one_rec += 1; } else { two_rec += 1; }
            }
        }
        if case.program.nodes.iter().any(|op| matches!(op, Op::TopK { .. })) { topk += 1; }
        if case.program.nodes.iter().any(|op| matches!(op, Op::Window { .. })) { window += 1; }
        for op in &case.program.nodes {
            let input = match op { Op::Window { input, .. } => *input as usize, _ => continue };
            if matches!(case.program.nodes[input], Op::Join { .. }) { window_join += 1; }
            if matches!(case.program.nodes[input], Op::Reduce { .. }) { window_reduce += 1; }
        }
        if drive::Case::generate_mint(seed).program.nodes.iter().any(|op| matches!(op, Op::Mint { .. })) { mint += 1; }
        let shapes = drive::Case::generate_recursive_shapes(seed);
        let first = &shapes.program.nodes[7];
        let second = &shapes.program.nodes[8];
        if shapes.program.nodes.iter().any(|op| matches!(op, Op::Mint { .. })) { recursive_mint += 1; }
        if shapes.program.nodes.iter().any(|op| matches!(op, Op::Antijoin { .. })) { recursive_antijoin += 1; }
        if matches!(first, Op::Mint { .. }) { mint_before_antijoin += 1; }
        if matches!(second, Op::Mint { .. }) { antijoin_before_mint += 1; }
        if shapes.program.nodes.iter().any(|op| matches!(op, Op::Antijoin { lk, .. } if lk.is_empty())) { empty_antijoin_key += 1; }
    }
    println!("seeds=1000 one_rec={one_rec} two_rec={two_rec} topk={topk} mint={mint} window={window} window_join={window_join} window_reduce={window_reduce} recursive_mint={recursive_mint} recursive_antijoin={recursive_antijoin} mint_before_antijoin={mint_before_antijoin} antijoin_before_mint={antijoin_before_mint} empty_antijoin_key={empty_antijoin_key}");
    assert!(one_rec > 0 && two_rec > 0 && topk > 0 && mint > 0 && window > 0 && window_join > 0 && window_reduce > 0);
    assert!(recursive_mint > 0 && recursive_antijoin > 0 && mint_before_antijoin > 0 && antijoin_before_mint > 0 && empty_antijoin_key > 0);
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
