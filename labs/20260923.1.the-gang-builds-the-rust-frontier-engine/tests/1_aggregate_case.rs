//! The grouped COUNT/SUM case: `job(id, team, cost)` -> `team, COUNT(*),
//! SUM(cost)`. Every committed frontier's snapshot and net signed output
//! compared against `3c_aggregate_expected.tsv` and `3d_aggregate_deltas.tsv`,
//! including the empty group and the rolled-back frontier.

mod support;

use lab::Program;
use lab::{Engine, Frontier, FrontierEngine, PlanNode, RelId, Sign, SourceChange};
use lab_20260923_1 as lab;
use support::{line, TsvGroups};

struct AggregateCase {
    engine: Engine,
    program: lab::ProgramId,
    job: RelId,
}

fn aggregate_case() -> AggregateCase {
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
    AggregateCase {
        engine,
        program,
        job,
    }
}

fn chg(row: (i64, i64, i64), sign: Sign, relation: RelId) -> SourceChange {
    SourceChange {
        relation,
        sign,
        row: vec![row.0, row.1, row.2],
    }
}

/// Snapshot fields as the oracle prints them: team, jobs, total_cost.
fn snapshot_fields(rows: &[(Vec<i64>, i64)]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|(row, _)| vec![row[0].to_string(), row[1].to_string(), row[2].to_string()])
        .collect()
}

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
    let snapshots = TsvGroups::load(&support::packet().join("3c_aggregate_expected.tsv"));
    let deltas = TsvGroups::load(&support::packet().join("3d_aggregate_deltas.tsv"));

    let AggregateCase {
        mut engine,
        program,
        job,
    } = aggregate_case();

    let steps: Vec<(&str, Vec<SourceChange>, bool)> = vec![
        (
            "0_initial",
            vec![
                chg((1, 10, 5), Sign::Plus, job),
                chg((2, 10, 7), Sign::Plus, job),
                chg((3, 20, 11), Sign::Plus, job),
            ],
            false,
        ),
        // Move job 3 from team 20 to team 10 and add job 4 to team 10.
        (
            "1_move_and_add",
            vec![
                chg((4, 10, 3), Sign::Plus, job),
                chg((3, 20, 11), Sign::Minus, job),
                chg((3, 10, 11), Sign::Plus, job),
            ],
            false,
        ),
        // Cost of job 2 crosses zero: 7 -> -7. Team total 26 -> 12.
        (
            "2_cross_zero",
            vec![
                chg((2, 10, 7), Sign::Minus, job),
                chg((2, 10, -7), Sign::Plus, job),
            ],
            false,
        ),
        (
            "3_delete_two",
            vec![
                chg((1, 10, 5), Sign::Minus, job),
                chg((4, 10, 3), Sign::Minus, job),
            ],
            false,
        ),
        // Deleting the last jobs of every group leaves zero groups.
        (
            "4_empty",
            vec![
                chg((2, 10, -7), Sign::Minus, job),
                chg((3, 10, 11), Sign::Minus, job),
            ],
            false,
        ),
        // Insert inside a transaction, then roll the transaction back.
        ("5_rollback", vec![chg((5, 30, 9), Sign::Plus, job)], true),
    ];

    for (name, changes, rolls_back) in steps {
        let committed_delta: Vec<Vec<String>> = if rolls_back {
            let mut tx = engine.begin();
            let inside = tx
                .apply(
                    program,
                    Frontier {
                        id: name.into(),
                        changes,
                    },
                )
                .expect("frontier inside transaction applies");
            // Team 30 appears inside the open transaction.
            assert_eq!(inside[0].changes.len(), 1);
            assert_eq!(inside[0].changes[0].row, vec![30, 1, 9]);
            assert_eq!(inside[0].changes[0].sign, Sign::Plus);
            tx.rollback().expect("transaction rolls back");
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

        // Net output change vs `3d_aggregate_deltas.tsv` (5_rollback is EMPTY).
        assert_eq!(
            committed_delta,
            deltas
                .rows(name)
                .iter()
                .map(|fields| line(name, fields))
                .collect::<Vec<_>>(),
            "{name}: net output change"
        );
        // Committed snapshot vs `3c_aggregate_expected.tsv` (4_empty and
        // 5_rollback have zero rows and no lines).
        let support_rows = engine
            .snapshot_support(program, "team_cost")
            .expect("snapshot");
        assert_eq!(
            snapshot_fields(&support_rows),
            snapshots.rows(name).to_vec(),
            "{name}: committed snapshot"
        );
    }
}

#[test]
fn duplicate_source_rows_keep_distinct_identity() {
    let AggregateCase {
        mut engine,
        program,
        job,
    } = aggregate_case();
    // Same (team, cost) value, two rows: two derivations, one group.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "dup".into(),
                changes: vec![
                    chg((6, 30, 9), Sign::Plus, job),
                    chg((7, 30, 9), Sign::Plus, job),
                ],
            },
        )
        .unwrap();
    assert_eq!(output[0].changes.len(), 1);
    assert_eq!(output[0].changes[0].row, vec![30, 2, 18]);

    // Two distinct row identities behind equal values.
    let ids = engine.source_row_ids(job).unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let (first, second) = (ids[0], ids[1]);
    assert_eq!(
        engine.source_row(job, first).unwrap(),
        Some(&[6, 30, 9][..])
    );
    assert_eq!(
        engine.source_row(job, second).unwrap(),
        Some(&[7, 30, 9][..])
    );

    // Deleting by value removes exactly one instance.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "undup".into(),
                changes: vec![chg((7, 30, 9), Sign::Minus, job)],
            },
        )
        .unwrap();
    // The group changed: retract the old aggregate row, add the new one.
    let changes = &output[0].changes;
    assert_eq!(changes.len(), 2);
    // Row-sorted output: (30,1,9) before (30,2,18).
    assert_eq!(
        (changes[0].sign, changes[0].row.clone()),
        (Sign::Plus, vec![30, 1, 9])
    );
    assert_eq!(
        (changes[1].sign, changes[1].row.clone()),
        (Sign::Minus, vec![30, 2, 18])
    );
    assert_eq!(engine.source_row(job, second).unwrap(), None);
    assert_eq!(
        engine.source_row(job, first).unwrap(),
        Some(&[6, 30, 9][..])
    );
}

#[test]
fn net_zero_batch_leaves_group_untouched() {
    let AggregateCase {
        mut engine,
        program,
        job,
    } = aggregate_case();
    engine
        .apply(
            program,
            Frontier {
                id: "seed".into(),
                changes: vec![chg((1, 10, 5), Sign::Plus, job)],
            },
        )
        .unwrap();
    let before = engine.snapshot_support(program, "team_cost").unwrap();
    // Add job 8 and remove an equal-valued job in the same frontier: the
    // group nets to zero and must not emit or change.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "net-zero".into(),
                changes: vec![
                    chg((8, 10, 5), Sign::Plus, job),
                    chg((1, 10, 5), Sign::Minus, job),
                ],
            },
        )
        .unwrap();
    assert!(
        output[0].changes.is_empty(),
        "net-zero aggregate batch emits nothing"
    );
    assert_eq!(
        engine.snapshot_support(program, "team_cost").unwrap(),
        before
    );
    assert_eq!(
        engine.stats().source_rows,
        1,
        "one row identity remains live"
    );
}

#[test]
fn empty_source_has_no_groups() {
    let AggregateCase {
        mut engine,
        program,
        job: _,
    } = aggregate_case();
    assert!(
        engine.snapshot(program, "team_cost").unwrap().is_empty(),
        "empty source, no groups"
    );
    // An empty frontier on an empty source stays empty and emits nothing.
    let output = engine.apply(program, Frontier::empty("vacuous")).unwrap();
    assert!(output[0].changes.is_empty());
    assert!(engine.snapshot(program, "team_cost").unwrap().is_empty());
}
