use std::{collections::BTreeMap, time::Instant};

use hafley_observe::{assert_growth_sized, Growth, SpanCounts};
use ivm_engine::{Engine, Raw};
use ivm_ir::{Expr, Frontier, LetRec, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;

fn recursive_program(strata: usize) -> Program {
    let mut program = Program {
        texts: vec![],
        rels: vec![Relation {
            id: 0,
            name: "edges".into(),
            cols: vec![Ty::Int, Ty::Int],
            kind: RelKind::Source,
        }],
        nodes: vec![],
        strata: vec![],
        outputs: (1..=strata as u32).collect(),
    };
    for i in 1..=strata {
        let id = i as u32;
        program.rels.push(Relation {
            id,
            name: format!("reach_{id}"),
            cols: vec![Ty::Int, Ty::Int],
            kind: RelKind::Derived,
        });
        let at = program.nodes.len() as u32;
        program.nodes.extend([
            Op::Get(0),
            Op::Get(id),
            Op::Join {
                inputs: vec![at + 1, at],
                equivalences: vec![vec![(0, 1), (1, 0)]],
            },
            Op::Mfp {
                input: at + 2,
                filter: vec![],
                map: vec![],
                project: vec![0, 3],
            },
            Op::Union(vec![at, at + 3]),
        ]);
        program.strata.push(Stratum::LetRec(LetRec {
            ids: vec![id],
            bodies: vec![at + 4],
            limit: None,
        }));
    }
    program
}

fn install_size(strata: usize) -> (usize, usize) {
    let program = recursive_program(strata);
    let db = Connection::open_in_memory().unwrap();
    let start = Instant::now();
    let _engine = Sqlite::install(&program, &mut Raw::with_connection(&db)).unwrap();
    let elapsed = start.elapsed();
    let mut statement = db
        .prepare(
            "SELECT type, count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' GROUP BY type",
        )
        .unwrap();
    let by_kind = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let objects = by_kind
        .iter()
        .map(|(_, count)| *count as usize)
        .sum::<usize>();
    eprintln!(
        "strata={strata} nodes={} objects={objects} kinds={by_kind:?} install={elapsed:?}",
        program.nodes.len()
    );
    (program.nodes.len(), objects)
}

#[test]
fn schema_objects_grow_linearly_with_independent_recursive_strata() {
    let (small_nodes, small_objects) = install_size(8);
    let (large_nodes, large_objects) = install_size(32);
    let counts = |objects| SpanCounts {
        entries: BTreeMap::from([("schema_objects".into(), objects)]),
        ..SpanCounts::default()
    };
    assert_growth_sized(
        &counts(small_objects),
        &counts(large_objects),
        "schema_objects",
        small_nodes,
        large_nodes,
        Growth::Linear,
    );
}

#[test]
fn output_install_keeps_transitive_strata_and_filters_other_sources() {
    let program = Program {
        texts: vec![],
        rels: vec![
            Relation {
                id: 0,
                name: "left_source".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "right_source".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 2,
                name: "left_output".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
            Relation {
                id: 3,
                name: "copy_output".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
            Relation {
                id: 4,
                name: "right_output".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![Op::Get(0), Op::Get(2), Op::Get(1)],
        strata: vec![
            Stratum::Let { id: 2, body: 0 },
            Stratum::Let { id: 3, body: 1 },
            Stratum::Let { id: 4, body: 2 },
        ],
        outputs: vec![2, 3, 4],
    };
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = Sqlite::install(&program, &mut host).unwrap();
    let delta = engine
        .settle(
            Frontier {
                changes: vec![
                    SourceChange {
                        rel: 0,
                        row: vec![7],
                        w: 1,
                    },
                    SourceChange {
                        rel: 1,
                        row: vec![9],
                        w: 1,
                    },
                ],
            },
            &mut host,
        )
        .unwrap();
    assert_eq!(
        delta.changes,
        vec![(2, vec![7], 1), (3, vec![7], 1), (4, vec![9], 1)]
    );
    assert_eq!(engine.snapshot(3, &mut host).unwrap(), vec![(vec![7], 1)]);
}

#[test]
fn output_install_remaps_text_literals() {
    let program = Program {
        texts: vec!["unused".into(), "first".into(), "second".into()],
        rels: vec![
            Relation {
                id: 0,
                name: "text_source".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "first_output".into(),
                cols: vec![Ty::Id],
                kind: RelKind::Derived,
            },
            Relation {
                id: 2,
                name: "second_output".into(),
                cols: vec![Ty::Id],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mfp {
                input: 0,
                filter: vec![],
                map: vec![Expr::Text(1)],
                project: vec![1],
            },
            Op::Mfp {
                input: 0,
                filter: vec![],
                map: vec![Expr::Text(2)],
                project: vec![1],
            },
        ],
        strata: vec![
            Stratum::Let { id: 1, body: 1 },
            Stratum::Let { id: 2, body: 2 },
        ],
        outputs: vec![1, 2],
    };
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = Sqlite::install(&program, &mut host).unwrap();
    engine
        .settle(
            Frontier {
                changes: vec![SourceChange {
                    rel: 0,
                    row: vec![1],
                    w: 1,
                }],
            },
            &mut host,
        )
        .unwrap();
    for (rel, text) in [(1, "first"), (2, "second")] {
        let id: i64 = db
            .query_row(
                "SELECT id FROM ivm_term_dict WHERE text=?1",
                [text],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            engine.snapshot(rel, &mut host).unwrap(),
            vec![(vec![id], 1)]
        );
    }
}

#[test]
fn c15_shaped_ir_installs_one_shared_plan() {
    // Captured from sprefa's c15_standard_plus_3 lowerer on 2026-09-27.
    let program: Program = serde_json::from_str(include_str!("corpus/8_c15_program.json")).unwrap();
    assert_eq!(
        (
            program.nodes.len(),
            program.strata.len(),
            program.outputs.len()
        ),
        (4011, 164, 93)
    );
    let db = Connection::open_in_memory().unwrap();
    let start = Instant::now();
    let engine = Sqlite::install(&program, &mut Raw::with_connection(&db)).unwrap();
    let elapsed = start.elapsed();
    let objects: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    eprintln!(
        "c15 nodes={} strata={} outputs={} objects={objects} install={elapsed:?}",
        program.nodes.len(),
        program.strata.len(),
        program.outputs.len()
    );
    assert!(
        objects < 8_000,
        "c15-shaped install created {objects} schema objects"
    );
    for &rel in [program.outputs[0], *program.outputs.last().unwrap()].iter() {
        assert!(engine
            .snapshot(rel, &mut Raw::with_connection(&db))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn bundled_outputs_keep_set_and_bag_weights() {
    let program = Program {
        texts: vec![],
        rels: vec![
            Relation {
                id: 0,
                name: "weight_source".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "set_output".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
            Relation {
                id: 2,
                name: "bag_output".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![Op::Get(0), Op::Threshold(0), Op::Union(vec![0, 0])],
        strata: vec![
            Stratum::Let { id: 1, body: 1 },
            Stratum::Let { id: 2, body: 2 },
        ],
        outputs: vec![1, 2],
    };
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = Sqlite::install(&program, &mut host).unwrap();
    let change = |w| Frontier {
        changes: vec![SourceChange {
            rel: 0,
            row: vec![7],
            w,
        }],
    };
    assert_eq!(
        engine.settle(change(1), &mut host).unwrap().changes,
        vec![(1, vec![7], 1), (2, vec![7], 2)]
    );
    assert_eq!(engine.snapshot(1, &mut host).unwrap(), vec![(vec![7], 1)]);
    assert_eq!(engine.snapshot(2, &mut host).unwrap(), vec![(vec![7], 2)]);
    assert_eq!(
        engine.settle(change(-1), &mut host).unwrap().changes,
        vec![(1, vec![7], -1), (2, vec![7], -2)]
    );
}
