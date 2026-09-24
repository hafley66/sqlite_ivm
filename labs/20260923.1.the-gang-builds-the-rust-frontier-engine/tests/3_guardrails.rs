//! Contractual guardrails: operator and arrangement counts, explicit
//! unsupported results for missing required shapes, and both packet cases
//! running through one engine with the same API.

use lab::Program;
use lab::{
    Engine, ErrorKind, Frontier, FrontierEngine, PlanNode, RelId, Sign, SourceChange, Stage,
};
use lab_20260923_1 as lab;

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

fn aggregate_program(job: RelId) -> Program {
    Program::one(
        "team_cost",
        PlanNode::Aggregate {
            input: Box::new(PlanNode::Scan { relation: job }),
            group_by: vec![1],
            count: true,
            sum: Some(2),
        },
    )
}

#[test]
fn operator_and_arrangement_counts_are_contractual() {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let direct_grant = engine.define_relation("direct_grant", 2);
    let job = engine.define_relation("job", 3);

    // Grant program: union, scan, project, join, scan, scan = 6 operators;
    // union counts + join (counts + 2 indexes) + project counts = 5.
    let grant = engine
        .install(&grant_program(membership, permission, direct_grant))
        .unwrap();
    let stats = engine.stats();
    assert_eq!(stats.installed_programs, 1);
    assert_eq!(stats.operators, 6, "one operator per plan node");
    assert_eq!(
        stats.arrangements, 5,
        "union, join counts, two join indexes, project"
    );

    // Aggregate program: aggregate + scan = 2 operators; group map = 1.
    let _aggregate = engine.install(&aggregate_program(job)).unwrap();
    let stats = engine.stats();
    assert_eq!(stats.installed_programs, 2);
    assert_eq!(stats.operators, 8);
    assert_eq!(stats.arrangements, 6);

    // Uninstall drops exactly that program's objects.
    engine.uninstall(grant).unwrap();
    let stats = engine.stats();
    assert_eq!(
        (
            stats.installed_programs,
            stats.operators,
            stats.arrangements
        ),
        (1, 2, 1)
    );
}

#[test]
fn difference_plan_is_explicitly_unsupported() {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let program = Program::one(
        "exclusive",
        PlanNode::Difference {
            minuend: Box::new(PlanNode::Scan {
                relation: membership,
            }),
            subtrahend: Box::new(PlanNode::Scan {
                relation: permission,
            }),
        },
    );
    let error = engine
        .install(&program)
        .expect_err("difference is unsupported");
    assert_eq!(error.stage, Stage::Install);
    assert_eq!(error.output.as_deref(), Some("exclusive"));
    assert!(matches!(
        error.kind,
        ErrorKind::Unsupported {
            shape: "difference"
        }
    ));
    // No success-shaped fallback: nothing was installed.
    assert_eq!(engine.stats().installed_programs, 0);
}

#[test]
fn global_aggregate_is_explicitly_unsupported() {
    let mut engine = Engine::new();
    let job = engine.define_relation("job", 3);
    let program = Program::one(
        "totals",
        PlanNode::Aggregate {
            input: Box::new(PlanNode::Scan { relation: job }),
            group_by: vec![],
            count: true,
            sum: Some(2),
        },
    );
    let error = engine
        .install(&program)
        .expect_err("global aggregate is unsupported");
    assert_eq!(error.stage, Stage::Install);
    assert_eq!(error.output.as_deref(), Some("totals"));
    assert!(matches!(
        error.kind,
        ErrorKind::Unsupported {
            shape: "global aggregate (no group key)"
        }
    ));
}

#[test]
fn both_cases_share_one_engine_and_one_api() {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    let permission = engine.define_relation("permission", 2);
    let direct_grant = engine.define_relation("direct_grant", 2);
    let job = engine.define_relation("job", 3);
    let grant = engine
        .install(&grant_program(membership, permission, direct_grant))
        .unwrap();
    let aggregate = engine.install(&aggregate_program(job)).unwrap();

    // One frontier format, one apply, one snapshot shape — different programs.
    let grant_delta = engine
        .apply(
            grant,
            Frontier {
                id: "grant-seed".into(),
                changes: vec![
                    SourceChange {
                        relation: membership,
                        sign: Sign::Plus,
                        row: vec![1, 10],
                    },
                    SourceChange {
                        relation: permission,
                        sign: Sign::Plus,
                        row: vec![10, 100],
                    },
                ],
            },
        )
        .unwrap();
    assert_eq!(grant_delta[0].changes.len(), 1);
    assert_eq!(grant_delta[0].output, "access");

    let aggregate_delta = engine
        .apply(
            aggregate,
            Frontier {
                id: "job-seed".into(),
                changes: vec![SourceChange {
                    relation: job,
                    sign: Sign::Plus,
                    row: vec![1, 10, 5],
                }],
            },
        )
        .unwrap();
    assert_eq!(aggregate_delta[0].changes.len(), 1);
    assert_eq!(aggregate_delta[0].changes[0].row, vec![10, 1, 5]);

    // Programs are isolated: the aggregate snapshot is untouched by grant
    // frontiers and vice versa.
    assert_eq!(
        engine.snapshot(aggregate, "team_cost").unwrap(),
        vec![vec![10, 1, 5]]
    );
    assert_eq!(
        engine.snapshot(grant, "access").unwrap(),
        vec![vec![1, 100]]
    );
    assert_eq!(engine.outputs(grant).unwrap(), vec!["access".to_owned()]);
    assert_eq!(
        engine.outputs(aggregate).unwrap(),
        vec!["team_cost".to_owned()]
    );
}

#[test]
fn install_rejects_arity_mismatches() {
    let mut engine = Engine::new();
    let membership = engine.define_relation("membership", 2);
    // Join column 1 does not exist in a 1-column scan side.
    let bogus = engine.define_relation("side", 1);
    let program = Program::one(
        "bad-join",
        PlanNode::Join {
            left: Box::new(PlanNode::Scan {
                relation: membership,
            }),
            right: Box::new(PlanNode::Scan { relation: bogus }),
            on_left: 1,
            on_right: 5,
        },
    );
    let error = engine
        .install(&program)
        .expect_err("join column out of range");
    assert_eq!(error.stage, Stage::Install);
    assert!(matches!(error.kind, ErrorKind::ArityMismatch { .. }));
}
