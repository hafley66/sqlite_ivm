//! Named scripts from plans/2026-09-24-ivm-cousins/5_dd_test_plan.md, two programs through one API (C1).

mod support;

use ivm_dd::Dd;

#[test]
fn s_a_access() {
    println!("{}", support::run::<Dd>("0_access"));
}

#[test]
fn s_g_team_cost() {
    println!("{}", support::run::<Dd>("1_team_cost"));
}

#[test]
fn s_w_weights_access() {
    println!("{}", support::run::<Dd>("2_weights_access"));
}

#[test]
fn s_w_weights_team_cost() {
    println!("{}", support::run::<Dd>("3_weights_team_cost"));
}

#[test]
fn s_n1_antijoin() {
    println!("{}", support::run::<Dd>("4_antijoin"));
}

#[test]
fn s_n4_self_join() {
    println!("{}", support::run::<Dd>("5_self_join"));
}

#[test]
fn s_n3_topk() {
    println!("{}", support::run::<Dd>("6_topk"));
}

#[test]
fn s_n2_reach() {
    println!("{}", support::run::<Dd>("7_reach"));
}

#[test]
fn s5_recursive_antijoin_outer_input() {
    println!("{}", support::run::<Dd>("11_recursive_antijoin"));
}

#[test]
fn recursive_antijoin_left_input() {
    println!("{}", support::run::<Dd>("14_recursive_antijoin"));
}

#[test]
fn nonmonotone_letrec_walk() {
    println!("{}", support::run::<Dd>("17_nonmonotone_walk"));
}

#[test]
fn recursive_mint_input() {
    println!("{}", support::run::<Dd>("15_recursive_mint"));
}

#[test]
fn s_t_s_i_timing_and_cells() {
    println!("{}", support::run::<Dd>("8_timing_and_cells"));
}

#[test]
fn s_n5_depth_cap() {
    println!("{}", support::run::<Dd>("9_depth_cap"));
}

#[test]
fn s_g_accumulable_team_sum() {
    println!("{}", support::run::<Dd>("10_team_sum"));
}

#[test]
fn window_functions() {
    println!("{}", support::run::<Dd>("12_window"));
}

#[test]
fn delay_requires_clock_checker() {
    support::expect_install_error::<Dd>("13_delay");
}

#[test]
fn pokemon_0_can_surf() {
    println!("{}", support::run::<Dd>("pokemon/0_can_surf"));
}

#[test]
fn pokemon_1_party_stats() {
    println!("{}", support::run::<Dd>("pokemon/1_party_stats"));
}

#[test]
fn pokemon_2_party_size() {
    println!("{}", support::run::<Dd>("pokemon/2_party_size"));
}

#[test]
fn pokemon_3_rematch() {
    println!("{}", support::run::<Dd>("pokemon/3_rematch"));
}

#[test]
fn pokemon_4_two_roads() {
    println!("{}", support::run::<Dd>("pokemon/4_two_roads"));
}

#[test]
fn pokemon_5_leads() {
    println!("{}", support::run::<Dd>("pokemon/5_leads"));
}

#[test]
fn pokemon_6_walk() {
    println!("{}", support::run::<Dd>("pokemon/6_walk"));
}

#[test]
fn pokemon_7_rare_candy() {
    println!("{}", support::run::<Dd>("pokemon/7_rare_candy"));
}
