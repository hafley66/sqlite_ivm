//! End-to-end walk of both packet cases through the in-process frontier
//! engine, plus repeated-frontier timing and a process RSS sample.
//!
//! Run: `cargo run --offline --example 0_frontiers`

use lab::Program;
use lab::{Engine, Frontier, FrontierEngine, PlanNode, Sign, SourceChange};
use lab_20260923_1 as lab;
use std::time::Instant;

fn main() {
    grant_case();
    aggregate_case();
    repeated_frontier_cost();
    let usage = hafley_observe::rusage::sample();
    let rss = usage
        .peak_rss_bytes
        .map(|bytes| format!("{} MiB", bytes / (1024 * 1024)))
        .unwrap_or_else(|| "unavailable".into());
    println!("\npeak RSS: {rss}");
}

fn grant_case() {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let direct_grant = engine.define_relation("direct_grant", 2);
    let program = Program::one(
        "access",
        PlanNode::Union {
            inputs: vec![
                PlanNode::Scan {
                    relation: direct_grant,
                },
                PlanNode::Project {
                    input: Box::new(PlanNode::Join {
                        left: Box::new(PlanNode::Scan {
                            relation: membership,
                        }),
                        right: Box::new(PlanNode::Scan {
                            relation: permission,
                        }),
                        on_left: 1,
                        on_right: 0,
                    }),
                    columns: vec![0, 3],
                },
            ],
        },
    );
    let program = engine.install(&program).expect("grant program installs");

    let chg = |relation, sign, a: i64, b: i64| SourceChange {
        relation,
        sign,
        row: vec![a, b],
    };
    let steps: Vec<(&str, Vec<SourceChange>)> = vec![
        (
            "0_initial",
            vec![
                chg(membership, Sign::Plus, 1, 10),
                chg(membership, Sign::Plus, 1, 20),
                chg(permission, Sign::Plus, 10, 100),
                chg(permission, Sign::Plus, 20, 100),
                chg(direct_grant, Sign::Plus, 3, 300),
            ],
        ),
        (
            "1_both_join_inputs",
            vec![
                chg(membership, Sign::Plus, 2, 10),
                chg(permission, Sign::Plus, 10, 200),
            ],
        ),
        (
            "2_duplicate_union_support",
            vec![chg(direct_grant, Sign::Plus, 1, 200)],
        ),
        (
            "3_join_support_retract",
            vec![chg(membership, Sign::Minus, 1, 10)],
        ),
        (
            "4_last_join_support",
            vec![chg(permission, Sign::Minus, 20, 100)],
        ),
        (
            "5_last_union_support",
            vec![chg(direct_grant, Sign::Minus, 1, 200)],
        ),
        (
            "8_update",
            vec![
                chg(permission, Sign::Minus, 10, 200),
                chg(permission, Sign::Plus, 10, 300),
            ],
        ),
    ];

    println!("== grant union-join case ==");
    for (name, changes) in steps {
        let outputs = engine
            .apply(
                program,
                Frontier {
                    id: name.into(),
                    changes,
                },
            )
            .expect("frontier applies");
        for delta in outputs {
            println!("{name}: {:?}", pretty(&delta));
        }
    }
    println!(
        "snapshot: {:?}",
        engine.snapshot(program, "access").unwrap()
    );
    println!("stats:    {:?}\n", engine.stats());
}

fn aggregate_case() {
    let mut engine = Engine::new();
    let job = engine.define_relation("job", 3);
    let program = Program::one(
        "team_cost",
        PlanNode::Aggregate {
            input: Box::new(PlanNode::Scan { relation: job }),
            group_by: vec![1],
            count: true,
            sum: Some(2),
        },
    );
    let program = engine
        .install(&program)
        .expect("aggregate program installs");

    let job = |row: [i64; 3], sign| SourceChange {
        relation: job,
        sign,
        row: row.to_vec(),
    };
    let steps: Vec<(&str, Vec<SourceChange>)> = vec![
        (
            "0_initial",
            vec![
                job([1, 10, 5], Sign::Plus),
                job([2, 10, 7], Sign::Plus),
                job([3, 20, 11], Sign::Plus),
            ],
        ),
        (
            "1_move_and_add",
            vec![
                job([4, 10, 3], Sign::Plus),
                job([3, 20, 11], Sign::Minus),
                job([3, 10, 11], Sign::Plus),
            ],
        ),
        (
            "2_cross_zero",
            vec![job([2, 10, 7], Sign::Minus), job([2, 10, -7], Sign::Plus)],
        ),
        (
            "3_delete_two",
            vec![job([1, 10, 5], Sign::Minus), job([4, 10, 3], Sign::Minus)],
        ),
        (
            "4_empty",
            vec![job([2, 10, -7], Sign::Minus), job([3, 10, 11], Sign::Minus)],
        ),
    ];

    println!("== grouped count/sum case ==");
    for (name, changes) in steps {
        let outputs = engine
            .apply(
                program,
                Frontier {
                    id: name.into(),
                    changes,
                },
            )
            .expect("frontier applies");
        for delta in outputs {
            println!("{name}: {:?}", pretty(&delta));
        }
    }
    println!(
        "snapshot: {:?} (all groups empty)",
        engine.snapshot(program, "team_cost").unwrap()
    );
    println!("stats:    {:?}\n", engine.stats());
}

fn repeated_frontier_cost() {
    let mut engine = Engine::new();
    let job = engine.define_relation("job", 3);
    let program = Program::one(
        "team_cost",
        PlanNode::Aggregate {
            input: Box::new(PlanNode::Scan { relation: job }),
            group_by: vec![1],
            count: true,
            sum: Some(2),
        },
    );
    let program = engine.install(&program).expect("installs");

    // 1000 alternating frontiers; each nets one live row in.
    const FRONTIERS: u32 = 1000;
    let start = Instant::now();
    for index in 0..FRONTIERS {
        engine
            .apply(
                program,
                Frontier {
                    id: format!("r{index}"),
                    changes: vec![SourceChange {
                        relation: job,
                        sign: Sign::Plus,
                        row: vec![index as i64, (index as i64) % 8, index as i64],
                    }],
                },
            )
            .expect("repeated frontier applies");
    }
    let elapsed = start.elapsed();
    println!(
        "== repeated frontiers ==\n{FRONTIERS} frontiers in {elapsed:?} ({:?}/frontier)",
        elapsed / FRONTIERS
    );
    println!("stats: {:?}", engine.stats());
}

fn pretty(delta: &lab::OutputDelta) -> String {
    let parts: Vec<String> = delta
        .changes
        .iter()
        .map(|change| {
            format!(
                "{}{:?}",
                if change.sign == Sign::Plus { "+" } else { "-" },
                change.row
            )
        })
        .collect();
    format!("{}: [{}]", delta.output, parts.join(", "))
}
