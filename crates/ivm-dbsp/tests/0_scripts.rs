//! The ivm-dd named scripts, run on the dbsp engine through the same generic harness.

extern crate ivm_dbsp as ivm_dd;

#[path = "../../ivm-dd/tests/support/mod.rs"]
mod support;

use ivm_dbsp::Dbsp;
#[test]
fn s_a_access() {
    println!("{}", support::run::<Dbsp>("0_access"));
}

#[test]
fn s_g_team_cost() {
    println!("{}", support::run::<Dbsp>("1_team_cost"));
}

#[test]
fn s_w_weights_access() {
    println!("{}", support::run::<Dbsp>("2_weights_access"));
}

#[test]
fn s_w_weights_team_cost() {
    println!("{}", support::run::<Dbsp>("3_weights_team_cost"));
}

#[test]
fn s_n1_antijoin() {
    println!("{}", support::run::<Dbsp>("4_antijoin"));
}

#[test]
fn s_n4_self_join() {
    println!("{}", support::run::<Dbsp>("5_self_join"));
}

#[test]
fn s_n3_topk() {
    println!("{}", support::run::<Dbsp>("6_topk"));
}

#[test]
fn s_n2_reach() {
    println!("{}", support::run::<Dbsp>("7_reach"));
}

#[test]
fn s5_recursive_antijoin_outer_input() {
    println!("{}", support::run::<Dbsp>("11_recursive_antijoin"));
}

#[test]
fn recursive_antijoin_left_input() {
    println!("{}", support::run::<Dbsp>("14_recursive_antijoin"));
}

#[test]
fn nonmonotone_letrec_walk() {
    println!("{}", support::run::<Dbsp>("17_nonmonotone_walk"));
}

#[test]
fn recursive_mint_input() {
    println!("{}", support::run::<Dbsp>("15_recursive_mint"));
}

#[test]
fn s_t_s_i_timing_and_cells() {
    println!("{}", support::run::<Dbsp>("8_timing_and_cells"));
}

#[test]
fn s_n5_depth_cap() {
    println!("{}", support::run::<Dbsp>("9_depth_cap"));
}

#[test]
fn s_g_accumulable_team_sum() {
    println!("{}", support::run::<Dbsp>("10_team_sum"));
}

#[test]
fn window_functions() {
    println!("{}", support::run::<Dbsp>("12_window"));
}

#[test]
fn delay_requires_clock_checker() {
    support::expect_install_error::<Dbsp>("13_delay");
}

#[test]
fn pokemon_0_can_surf() {
    println!("{}", support::run::<Dbsp>("pokemon/0_can_surf"));
}

#[test]
fn pokemon_1_party_stats() {
    println!("{}", support::run::<Dbsp>("pokemon/1_party_stats"));
}

#[test]
fn pokemon_2_party_size() {
    println!("{}", support::run::<Dbsp>("pokemon/2_party_size"));
}

#[test]
fn pokemon_3_rematch() {
    println!("{}", support::run::<Dbsp>("pokemon/3_rematch"));
}

#[test]
fn pokemon_4_two_roads() {
    println!("{}", support::run::<Dbsp>("pokemon/4_two_roads"));
}

#[test]
fn pokemon_5_leads() {
    println!("{}", support::run::<Dbsp>("pokemon/5_leads"));
}

#[test]
fn pokemon_6_walk() {
    println!("{}", support::run::<Dbsp>("pokemon/6_walk"));
}

#[test]
fn pokemon_7_rare_candy() {
    println!("{}", support::run::<Dbsp>("pokemon/7_rare_candy"));
}

#[test]
fn nested_letrec_wave_reach() {
    println!("{}", support::run::<Dbsp>("18_nested_wave_reach"));
}
