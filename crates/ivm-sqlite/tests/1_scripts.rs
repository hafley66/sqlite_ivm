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

#[test]
fn self_join_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("5_self_join"));
}

#[test]
fn timing_and_cells_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("8_timing_and_cells"));
}

#[test]
fn reach_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("7_reach"));
}

#[test]
fn team_cost_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("1_team_cost"));
}

#[test]
fn topk_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("6_topk"));
}

#[test]
fn pokemon_walk_script_through_promoted_sqlite_engine() {
    println!("{}", support::run::<Sqlite>("pokemon/6_walk"));
}

macro_rules! script_case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            println!("{}", support::run::<Sqlite>($name));
        }
    };
}

script_case!(weights_access, "2_weights_access");
script_case!(weights_team_cost, "3_weights_team_cost");
script_case!(depth_cap, "9_depth_cap");
script_case!(pokemon_can_surf, "pokemon/0_can_surf");
script_case!(pokemon_party_stats, "pokemon/1_party_stats");
script_case!(pokemon_party_size, "pokemon/2_party_size");
script_case!(pokemon_rematch, "pokemon/3_rematch");
script_case!(pokemon_two_roads, "pokemon/4_two_roads");
script_case!(pokemon_leads, "pokemon/5_leads");
script_case!(pokemon_rare_candy, "pokemon/7_rare_candy");
