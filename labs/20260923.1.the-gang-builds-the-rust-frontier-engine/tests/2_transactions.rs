//! Transaction scopes: savepoint and transaction rollback leave the previous
//! committed snapshot readable; a failing frontier unwinds by itself; an
//! unsettled scope rolls back on drop.

use lab::Program;
use lab::{
    Engine, ErrorKind, Frontier, FrontierEngine, PlanNode, RelId, Sign, SourceChange, Stage,
};
use lab_20260923_1 as lab;

fn grant_case() -> (Engine, lab::ProgramId, RelId, RelId, RelId) {
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
    let program = engine.install(&program).expect("installs");
    (engine, program, membership, permission, direct_grant)
}

fn chg(relation: RelId, sign: Sign, a: i64, b: i64) -> SourceChange {
    SourceChange {
        relation,
        sign,
        row: vec![a, b],
    }
}

fn seed(
    engine: &mut Engine,
    program: lab::ProgramId,
    membership: RelId,
    permission: RelId,
    direct_grant: RelId,
) {
    engine
        .apply(
            program,
            Frontier {
                id: "0_initial".into(),
                changes: vec![
                    chg(membership, Sign::Plus, 1, 10),
                    chg(membership, Sign::Plus, 1, 20),
                    chg(permission, Sign::Plus, 10, 100),
                    chg(permission, Sign::Plus, 20, 100),
                    chg(direct_grant, Sign::Plus, 3, 300),
                ],
            },
        )
        .unwrap();
}

#[test]
fn error_mid_frontier_leaves_committed_state_readable() {
    // The in-process analogue of a callback error: the frontier fails at
    // validation, unwinds, and the committed snapshot stays readable.
    let (mut engine, program, membership, permission, direct_grant) = grant_case();
    seed(&mut engine, program, membership, permission, direct_grant);
    let committed = engine.snapshot_support(program, "access").unwrap();

    let mut tx = engine.begin();
    // The second change fails on arity: membership takes 2 cells, this row
    // has 3. The first change must unwind with it.
    let result = tx.apply(
        program,
        Frontier {
            id: "bad".into(),
            changes: vec![
                chg(direct_grant, Sign::Plus, 4, 400),
                SourceChange {
                    relation: membership,
                    sign: Sign::Plus,
                    row: vec![9, 9, 9],
                },
            ],
        },
    );
    let error = result.expect_err("arity mismatch fails the frontier");
    assert_eq!(error.stage, Stage::Validate);
    assert_eq!(error.relation.as_deref(), Some("membership"));
    assert!(matches!(
        error.kind,
        ErrorKind::ArityMismatch {
            expected: 2,
            actual: 3
        }
    ));

    // The committed snapshot is readable inside the still-open transaction.
    assert_eq!(tx.snapshot_support(program, "access").unwrap(), committed);
    tx.commit().unwrap();
    // And after commit: (4,400) did not slip through with the failed frontier.
    assert_eq!(
        engine.snapshot_support(program, "access").unwrap(),
        committed
    );
}

#[test]
fn unknown_relation_error_names_stage_and_relation() {
    let (mut engine, program, _membership, _permission, _direct_grant) = grant_case();
    let ghost = RelId(99);
    let error = engine
        .apply(
            program,
            Frontier {
                id: "ghost".into(),
                changes: vec![chg(ghost, Sign::Plus, 1, 2)],
            },
        )
        .expect_err("unknown relation");
    assert_eq!(error.stage, Stage::Validate);
    assert!(matches!(error.kind, ErrorKind::UnknownRelation(RelId(99))));
    let text = error.to_string();
    assert!(text.contains("Validate"), "error names the stage: {text}");
}

#[test]
fn removal_past_multiplicity_fails_before_writes() {
    let (mut engine, program, membership, permission, direct_grant) = grant_case();
    seed(&mut engine, program, membership, permission, direct_grant);
    let committed = engine.snapshot_support(program, "access").unwrap();
    // membership(1,10) exists once; removing it twice in one frontier must
    // fail validation before any write.
    let error = engine
        .apply(
            program,
            Frontier {
                id: "over-delete".into(),
                changes: vec![
                    chg(membership, Sign::Minus, 1, 10),
                    chg(membership, Sign::Minus, 1, 10),
                ],
            },
        )
        .expect_err("removal exceeds multiplicity");
    assert_eq!(error.stage, Stage::Validate);
    assert_eq!(
        engine.snapshot_support(program, "access").unwrap(),
        committed
    );
    // The engine still accepts valid work afterwards.
    let output = engine
        .apply(
            program,
            Frontier {
                id: "ok".into(),
                changes: vec![chg(direct_grant, Sign::Plus, 4, 400)],
            },
        )
        .unwrap();
    assert_eq!(output[0].changes.len(), 1);
}

#[test]
fn savepoint_rollback_discards_only_its_own_work() {
    let (mut engine, program, membership, permission, direct_grant) = grant_case();
    seed(&mut engine, program, membership, permission, direct_grant);
    let before = engine.snapshot_support(program, "access").unwrap();

    let mut tx = engine.begin();
    // Committed-inside-tx work before the savepoint.
    tx.apply(
        program,
        Frontier {
            id: "tx-add".into(),
            changes: vec![chg(direct_grant, Sign::Plus, 7, 700)],
        },
    )
    .unwrap();
    let with_seven = tx.snapshot_support(program, "access").unwrap();
    assert_eq!(with_seven.len(), before.len() + 1);

    // Savepoint work that will be discarded.
    let mut savepoint = tx.savepoint();
    savepoint
        .apply(
            program,
            Frontier {
                id: "sp-add".into(),
                changes: vec![chg(direct_grant, Sign::Plus, 8, 800)],
            },
        )
        .unwrap();
    savepoint.rollback_to().unwrap();
    assert_eq!(tx.snapshot_support(program, "access").unwrap(), with_seven);
    tx.commit().unwrap();

    let after = engine.snapshot_support(program, "access").unwrap();
    assert!(after.contains(&(vec![7, 700], 1)));
    assert!(!after.contains(&(vec![8, 800], 1)));
}

#[test]
fn unsettled_scope_rolls_back_on_drop() {
    let (mut engine, program, membership, permission, direct_grant) = grant_case();
    seed(&mut engine, program, membership, permission, direct_grant);
    let committed = engine.snapshot_support(program, "access").unwrap();
    {
        let mut tx = engine.begin();
        tx.apply(
            program,
            Frontier {
                id: "dropped".into(),
                changes: vec![chg(direct_grant, Sign::Plus, 9, 900)],
            },
        )
        .unwrap();
        // Dropped without commit or rollback: rolls back.
    }
    assert_eq!(
        engine.snapshot_support(program, "access").unwrap(),
        committed
    );
}

#[test]
fn uninstall_drops_arrangements_and_invalidates_the_program() {
    let (mut engine, program, membership, permission, direct_grant) = grant_case();
    seed(&mut engine, program, membership, permission, direct_grant);
    let stats_with = engine.stats();
    assert_eq!(stats_with.installed_programs, 1);
    engine.uninstall(program).unwrap();
    let stats_without = engine.stats();
    assert_eq!(stats_without.installed_programs, 0);
    assert_eq!(stats_without.operators, 0);
    assert_eq!(stats_without.arrangements, 0);
    // Source rows survive program teardown.
    assert_eq!(stats_without.source_rows, 5);
    let error = engine
        .snapshot(program, "access")
        .expect_err("program uninstalled");
    assert!(matches!(error.kind, ErrorKind::UnknownProgram(_)));
}
