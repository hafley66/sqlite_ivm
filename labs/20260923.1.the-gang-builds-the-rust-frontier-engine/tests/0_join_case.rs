//! The grant-union-join case: every committed frontier's snapshot and net
//! signed output compared against `3_expected.tsv` and `3a_deltas.tsv`,
//! including the rolled-back frontiers and the no-output ones.

mod support;

use lab::Program;
use lab::{Engine, Frontier, FrontierEngine, PlanNode, RelId, Sign, SourceChange};
use lab_20260923_1 as lab;
use support::{line, TsvGroups};

struct GrantCase {
    engine: Engine,
    program: lab::ProgramId,
    membership: RelId,
    permission: RelId,
    direct_grant: RelId,
}

fn grant_case() -> GrantCase {
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
    GrantCase {
        engine,
        program,
        membership,
        permission,
        direct_grant,
    }
}

fn change(relation: RelId, sign: Sign, person: i64, second: i64) -> SourceChange {
    SourceChange {
        relation,
        sign,
        row: vec![person, second],
    }
}

/// Snapshot fields as the oracle prints them: person, resource, weight.
fn snapshot_fields(rows: &[(Vec<i64>, i64)]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|(row, support)| vec![row[0].to_string(), row[1].to_string(), support.to_string()])
        .collect()
}

/// Committed delta fields: person, resource, signed weight.
fn delta_lines(frontier: &str, delta: &lab::OutputDelta) -> Vec<Vec<String>> {
    delta
        .changes
        .iter()
        .map(|change| {
            let weight = change.sign.weight();
            line(frontier, &TsvGroups::delta_fields(&change.row, weight))
        })
        .collect()
}

#[test]
fn every_committed_frontier_matches_both_tsvs() {
    let snapshots = TsvGroups::load(&support::packet().join("3_expected.tsv"));
    let deltas = TsvGroups::load(&support::packet().join("3a_deltas.tsv"));

    let GrantCase {
        mut engine,
        program,
        membership,
        permission,
        direct_grant,
    } = grant_case();

    // (frontier label, changes, rolls back) — the oracle sequence, with the
    // savepoint and transaction frontiers flagged.
    let steps: Vec<(&str, Vec<SourceChange>, bool)> = vec![
        (
            "0_initial",
            vec![
                change(membership, Sign::Plus, 1, 10),
                change(membership, Sign::Plus, 1, 20),
                change(permission, Sign::Plus, 10, 100),
                change(permission, Sign::Plus, 20, 100),
                change(direct_grant, Sign::Plus, 3, 300),
            ],
            false,
        ),
        (
            "1_both_join_inputs",
            vec![
                change(membership, Sign::Plus, 2, 10),
                change(permission, Sign::Plus, 10, 200),
            ],
            false,
        ),
        (
            "2_duplicate_union_support",
            vec![change(direct_grant, Sign::Plus, 1, 200)],
            false,
        ),
        (
            "3_join_support_retract",
            vec![change(membership, Sign::Minus, 1, 10)],
            false,
        ),
        (
            "4_last_join_support",
            vec![change(permission, Sign::Minus, 20, 100)],
            false,
        ),
        (
            "5_last_union_support",
            vec![change(direct_grant, Sign::Minus, 1, 200)],
            false,
        ),
        (
            "6_savepoint_rollback",
            vec![change(direct_grant, Sign::Plus, 4, 400)],
            true,
        ),
        (
            "7_transaction_rollback",
            vec![change(membership, Sign::Plus, 5, 10)],
            true,
        ),
        (
            "8_update",
            vec![
                change(permission, Sign::Minus, 10, 200),
                change(permission, Sign::Plus, 10, 300),
            ],
            false,
        ),
    ];

    for (name, changes, rolls_back) in steps {
        let committed_delta: Vec<Vec<String>> = if rolls_back {
            // Frontier 6: add inside a savepoint, roll back to it.
            // Frontier 7: add inside a transaction, roll the transaction back.
            let mut tx = engine.begin();
            if name == "6_savepoint_rollback" {
                let mut savepoint = tx.savepoint();
                let inside = savepoint
                    .apply(
                        program,
                        Frontier {
                            id: name.into(),
                            changes,
                        },
                    )
                    .expect("frontier inside savepoint applies");
                // (4,400) is visible inside the savepoint.
                assert_eq!(inside.len(), 1);
                assert_eq!(inside[0].changes.len(), 1);
                assert_eq!(inside[0].changes[0].row, vec![4, 400]);
                assert_eq!(inside[0].changes[0].sign, Sign::Plus);
                savepoint.rollback_to().expect("savepoint rolls back");
            } else {
                let inside = tx
                    .apply(
                        program,
                        Frontier {
                            id: name.into(),
                            changes,
                        },
                    )
                    .expect("frontier inside transaction applies");
                // p(10,100) and p(10,200) are both live here: two joins.
                let rows: Vec<Vec<i64>> = inside[0]
                    .changes
                    .iter()
                    .map(|change| change.row.clone())
                    .collect();
                assert_eq!(rows, vec![vec![5, 100], vec![5, 200]]);
                tx.rollback().expect("transaction rolls back");
                assert_deltas_and_snapshot(&engine, program, name, &deltas, &snapshots, &[]);
                continue;
            }
            tx.commit().expect("transaction commits empty");
            Vec::new()
        } else {
            let outputs = engine
                .apply(
                    program,
                    Frontier {
                        id: name.into(),
                        changes,
                    },
                )
                .unwrap_or_else(|error| panic!("{name}: apply failed: {error}"));
            assert_eq!(outputs.len(), 1, "{name}: one output");
            delta_lines(name, &outputs[0])
        };

        assert_deltas_and_snapshot(
            &engine,
            program,
            name,
            &deltas,
            &snapshots,
            &committed_delta,
        );
    }
}

fn assert_deltas_and_snapshot(
    engine: &Engine,
    program: lab::ProgramId,
    name: &str,
    deltas: &TsvGroups,
    snapshots: &TsvGroups,
    committed_delta: &[Vec<String>],
) {
    // Signed output frontier vs `3a_deltas.tsv` (EMPTY frontiers included).
    assert_eq!(
        committed_delta,
        deltas
            .rows(name)
            .iter()
            .map(|fields| line(name, fields))
            .collect::<Vec<_>>(),
        "{name}: net output change"
    );
    // Committed snapshot with support vs `3_expected.tsv`.
    let support_rows = engine
        .snapshot_support(program, "access")
        .expect("snapshot");
    assert_eq!(
        snapshot_fields(&support_rows),
        snapshots.rows(name).to_vec(),
        "{name}: snapshot"
    );
}

#[test]
fn support_transitions() {
    let GrantCase {
        mut engine,
        program,
        membership,
        permission,
        direct_grant,
    } = grant_case();
    let front = |id: &str, changes: Vec<SourceChange>| Frontier {
        id: id.into(),
        changes,
    };
    engine
        .apply(
            program,
            front(
                "f0",
                vec![
                    change(membership, Sign::Plus, 1, 10),
                    change(membership, Sign::Plus, 1, 20),
                    change(permission, Sign::Plus, 10, 100),
                    change(permission, Sign::Plus, 20, 100),
                    change(direct_grant, Sign::Plus, 3, 300),
                ],
            ),
        )
        .unwrap();
    // Support 2 -> 1: retract membership(1,10) while membership(1,20) still
    // derives (1,100). No output change.
    let output = engine
        .apply(
            program,
            front("f3", vec![change(membership, Sign::Minus, 1, 10)]),
        )
        .unwrap();
    assert!(
        output[0].changes.is_empty(),
        "2->1 support transition emits nothing"
    );
    assert_eq!(
        engine.snapshot(program, "access").unwrap(),
        vec![vec![1, 100], vec![3, 300]]
    );
    // Support 1 -> 0 emits exactly one retraction.
    let output = engine
        .apply(
            program,
            front("f4", vec![change(permission, Sign::Minus, 20, 100)]),
        )
        .unwrap();
    assert_eq!(output[0].changes.len(), 1);
    assert_eq!(
        (output[0].changes[0].sign, output[0].changes[0].row.clone()),
        (Sign::Minus, vec![1, 100])
    );
}

#[test]
fn simultaneous_join_inputs_produce_cross_term_once() {
    let GrantCase {
        mut engine,
        program,
        membership,
        permission,
        direct_grant,
    } = grant_case();
    engine
        .apply(
            program,
            Frontier {
                id: "seed".into(),
                changes: vec![
                    change(membership, Sign::Plus, 1, 10),
                    change(permission, Sign::Plus, 10, 100),
                    change(direct_grant, Sign::Plus, 3, 300),
                ],
            },
        )
        .unwrap();
    // membership(2,10) x permission(10,200) must appear exactly once.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "both".into(),
                changes: vec![
                    change(membership, Sign::Plus, 2, 10),
                    change(permission, Sign::Plus, 10, 200),
                ],
            },
        )
        .unwrap();
    let rows: Vec<(Vec<i64>, i64)> = output[0]
        .changes
        .iter()
        .map(|change| (change.row.clone(), change.sign.weight()))
        .collect();
    assert_eq!(
        rows,
        vec![(vec![1, 200], 1), (vec![2, 100], 1), (vec![2, 200], 1)],
        "cross-term (2,200) once, no duplicate"
    );
}

#[test]
fn update_is_retraction_plus_addition() {
    let GrantCase {
        mut engine,
        program,
        membership,
        permission,
        direct_grant: _,
    } = grant_case();
    engine
        .apply(
            program,
            Frontier {
                id: "seed".into(),
                changes: vec![
                    change(membership, Sign::Plus, 2, 10),
                    change(permission, Sign::Plus, 10, 200),
                ],
            },
        )
        .unwrap();
    let output = engine
        .apply(
            program,
            Frontier {
                id: "update".into(),
                changes: vec![
                    change(permission, Sign::Minus, 10, 200),
                    change(permission, Sign::Plus, 10, 300),
                ],
            },
        )
        .unwrap();
    assert_eq!(output[0].changes.len(), 2);
    assert_eq!(
        (output[0].changes[0].sign, output[0].changes[0].row.clone()),
        (Sign::Minus, vec![2, 200])
    );
    assert_eq!(
        (output[0].changes[1].sign, output[0].changes[1].row.clone()),
        (Sign::Plus, vec![2, 300])
    );
}

#[test]
fn net_zero_batch_emits_nothing_and_keeps_state() {
    let GrantCase {
        mut engine,
        program,
        membership,
        permission,
        direct_grant: _,
    } = grant_case();
    engine
        .apply(
            program,
            Frontier {
                id: "seed".into(),
                changes: vec![
                    change(membership, Sign::Plus, 2, 10),
                    change(permission, Sign::Plus, 10, 200),
                ],
            },
        )
        .unwrap();
    let before = engine.snapshot_support(program, "access").unwrap();
    let stats_before = engine.stats();
    // +row then -row in one frontier nets to zero at consolidation.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "net-zero".into(),
                changes: vec![
                    change(membership, Sign::Plus, 9, 10),
                    change(membership, Sign::Minus, 9, 10),
                ],
            },
        )
        .unwrap();
    assert!(output[0].changes.is_empty(), "net-zero batch emits nothing");
    assert_eq!(engine.snapshot_support(program, "access").unwrap(), before);
    assert_eq!(engine.stats().source_rows, stats_before.source_rows);
}
