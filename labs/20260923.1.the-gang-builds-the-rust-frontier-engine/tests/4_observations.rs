//! Observations through `hafley-observe`: frontier and maintain spans,
//! install events with guardrail counts, and per-frontier work that stays
//! constant across repeated frontiers.

use hafley_observe::{observed_growth, CountRecorder, Growth};
use lab::Program;
use lab::{Engine, Frontier, FrontierEngine, PlanNode, RelId, Sign, SourceChange};
use lab_20260923_1 as lab;
use tracing_subscriber::prelude::*;

const TARGET: &str = "frontier_engine";

fn grant_program(membership: RelId, permission: RelId, direct_grant: RelId) -> Program {
    Program::one(
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
    )
}

fn grant_case() -> (Engine, lab::ProgramId, RelId, RelId, RelId) {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let direct_grant = engine.define_relation("direct_grant", 2);
    let program = engine
        .install(&grant_program(membership, permission, direct_grant))
        .expect("installs");
    (engine, program, membership, permission, direct_grant)
}

fn chg(relation: RelId, sign: Sign, a: i64, b: i64) -> SourceChange {
    SourceChange {
        relation,
        sign,
        row: vec![a, b],
    }
}

#[test]
fn frontier_and_maintain_spans_are_recorded() {
    let (recorder, layer) = CountRecorder::new();
    let (mut engine, program, membership, permission, direct_grant) = grant_case();

    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        engine
            .apply(
                program,
                Frontier {
                    id: "seed".into(),
                    changes: vec![
                        chg(membership, Sign::Plus, 1, 10),
                        chg(permission, Sign::Plus, 10, 100),
                        chg(direct_grant, Sign::Plus, 3, 300),
                    ],
                },
            )
            .unwrap();
        engine
            .apply(
                program,
                Frontier {
                    id: "second".into(),
                    changes: vec![
                        chg(membership, Sign::Plus, 2, 10),
                        chg(permission, Sign::Plus, 10, 200),
                    ],
                },
            )
            .unwrap();
        // An empty frontier still records its frontier span.
        engine.apply(program, Frontier::empty("vacuous")).unwrap();
    });

    let counts = recorder.counts();
    // One frontier span per apply.
    counts.assert_instances("frontier", 3);
    // Maintain spans: union + join + project per nonempty frontier; the
    // empty frontier touches no program dependencies and maintains nothing.
    let maintain = counts.entries_of("maintain");
    assert_eq!(maintain, 6, "union+join+project per nonempty frontier");
}

#[test]
fn install_event_carries_guardrail_counts() {
    let (recorder, layer) = CountRecorder::new();
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let direct_grant = engine.define_relation("direct_grant", 2);
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        engine
            .install(&grant_program(membership, permission, direct_grant))
            .unwrap();
    });
    // Sum the `operators` field over install events at the root.
    let sums = recorder.event_sums(TARGET, tracing::Level::INFO, "", "operators", None);
    let root = sums
        .get(&(String::new(), String::new()))
        .expect("install event at root");
    assert_eq!(
        root.sum_of("operators"),
        6.0,
        "union, scan, project, join, scan, scan"
    );
    assert_eq!(root.sum_of("arrangements"), 5.0);
    assert_eq!(root.sum_of("outputs"), 1.0);
}

#[test]
fn repeated_frontier_work_is_constant_per_frontier() {
    // 11 vs 101 frontiers (seed + N): each frontier carries a real net
    // change, so total maintain entries must grow linearly with the frontier
    // count and per-frontier work must not depend on accumulated state.
    let run = |repeats: u64| {
        let (recorder, layer) = CountRecorder::new();
        let (mut engine, program, membership, permission, direct_grant) = grant_case();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            engine
                .apply(
                    program,
                    Frontier {
                        id: "seed".into(),
                        changes: vec![
                            chg(membership, Sign::Plus, 1, 10),
                            chg(permission, Sign::Plus, 10, 100),
                        ],
                    },
                )
                .unwrap();
            for index in 0..repeats {
                engine
                    .apply(
                        program,
                        Frontier {
                            id: format!("repeat-{index}"),
                            changes: vec![chg(direct_grant, Sign::Plus, 100 + index as i64, 200)],
                        },
                    )
                    .unwrap();
            }
        });
        recorder.counts()
    };
    let small = run(10);
    let large = run(100);
    let small_per_frontier = small.entries_of("maintain") as f64 / 11.0;
    let large_per_frontier = large.entries_of("maintain") as f64 / 101.0;
    assert_eq!(
        small_per_frontier, large_per_frontier,
        "identical work per frontier"
    );
    assert_eq!(
        observed_growth(&small, &large, "maintain", 101.0 / 11.0),
        Growth::Linear,
        "total work scales with frontier count, not with accumulated state"
    );
    // Frontier spans: 1 seed + N repeats.
    assert_eq!(small.entries_of("frontier"), 11);
    assert_eq!(large.entries_of("frontier"), 101);
}
