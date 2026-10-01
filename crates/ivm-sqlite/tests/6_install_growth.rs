#[path = "../../ivm-dd/tests/random/mod.rs"]
#[allow(dead_code)]
mod random;

use std::{collections::BTreeMap, ffi::{c_int, c_void, CStr}, time::Instant};

use hafley_observe::{assert_growth_sized, Growth, SpanCounts};
use ivm_dd::Dd;
use ivm_engine::{Engine, Raw};
use ivm_ir::{Expr, Frontier, Func, LetRec, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;

extern "C" fn count_create(_event: u32, context: *mut c_void, statement: *mut c_void, _extra: *mut c_void) -> c_int {
    let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement.cast()) };
    if !sql.is_null() {
        let sql = unsafe { CStr::from_ptr(sql) }.to_bytes();
        if sql.iter().take(6).copied().eq(b"CREATE".iter().copied()) {
            unsafe { *(context as *mut usize) += 1; }
        }
    }
    0
}

fn counted_install(program: &Program, db: &Connection) -> (Sqlite, usize) {
    let mut creates = 0usize;
    let handle = unsafe { db.handle() };
    let result = unsafe { rusqlite::ffi::sqlite3_trace_v2(
        handle,
        rusqlite::ffi::SQLITE_TRACE_STMT as u32,
        Some(count_create),
        (&mut creates as *mut usize).cast(),
    ) };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    let installed = Sqlite::install(program, &mut Raw::with_connection(db));
    unsafe { rusqlite::ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut()); }
    (installed.unwrap(), creates)
}

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
    let stateful_nodes = program.outputs.len()
        + program.strata.iter().filter(|stratum| matches!(stratum, Stratum::LetRec(_))).count()
        + program.nodes.iter().filter(|op| matches!(op,
            Op::Join { .. } | Op::Antijoin { .. } | Op::Reduce { .. }
            | Op::TopK { .. } | Op::Window { .. } | Op::Mint { .. })).count();
    let db = Connection::open_in_memory().unwrap();
    let start = Instant::now();
    let (_engine, creates) = counted_install(&program, &db);
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
        "strata={strata} nodes={} creates={creates} objects={objects} kinds={by_kind:?} install={elapsed:?}",
        program.nodes.len()
    );
    (stateful_nodes, creates)
}

#[test]
fn schema_objects_grow_linearly_with_independent_recursive_strata() {
    let (small_stateful, small_creates) = install_size(8);
    let (large_stateful, large_creates) = install_size(32);
    let counts = |creates| SpanCounts {
        entries: BTreeMap::from([("create_statements".into(), creates)]),
        ..SpanCounts::default()
    };
    assert_growth_sized(
        &counts(small_creates),
        &counts(large_creates),
        "create_statements",
        small_stateful,
        large_stateful,
        Growth::Linear,
    );
}

#[test]
fn pure_delta_chain_has_fixed_create_count() {
    let program = Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "source".into(), cols: vec![Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "output".into(), cols: vec![Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mfp { input: 0, filter: vec![Expr::Call(Func::Gt, vec![Expr::Col(0), Expr::Lit(0)])], map: vec![], project: vec![] },
            Op::Mfp { input: 1, filter: vec![], map: vec![Expr::Call(Func::Add, vec![Expr::Col(0), Expr::Lit(1)])], project: vec![1] },
            Op::Threshold(2),
        ],
        strata: vec![Stratum::Let { id: 1, body: 3 }],
        outputs: vec![1],
    };
    let db = Connection::open_in_memory().unwrap();
    let (mut engine, creates) = counted_install(&program, &db);
    eprintln!("pure chain creates={creates}");
    assert_eq!(creates, 22); // includes the ivm_term_sortkey dictionary table
    let mut host = Raw::with_connection(&db);
    assert_eq!(engine.settle(Frontier { changes: vec![SourceChange { rel: 0, row: vec![2], w: 1 }] }, &mut host).unwrap().changes,
        vec![(1, vec![3], 1)]);
    assert_eq!(engine.snapshot(1, &mut host).unwrap(), vec![(vec![3], 1)]);
    let mut extended = program.clone();
    extended.nodes.pop();
    for _ in 0..20 {
        let input = (extended.nodes.len() - 1) as u32;
        extended.nodes.push(Op::Mfp {
            input,
            filter: vec![Expr::Call(Func::Gt, vec![Expr::Col(0), Expr::Lit(0)])],
            map: vec![],
            project: vec![],
        });
    }
    let input = (extended.nodes.len() - 1) as u32;
    extended.nodes.push(Op::Threshold(input));
    extended.strata[0] = Stratum::Let { id: 1, body: input + 1 };
    let db = Connection::open_in_memory().unwrap();
    let (_, extended_creates) = counted_install(&extended, &db);
    assert_eq!(extended_creates, creates);
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
                "SELECT id FROM ivm_text WHERE text=?1",
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
    let (engine, creates) = counted_install(&program, &db);
    let elapsed = start.elapsed();
    let objects: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    eprintln!(
        "c15 nodes={} strata={} outputs={} creates={creates} objects={objects} install={elapsed:?}",
        program.nodes.len(),
        program.strata.len(),
        program.outputs.len()
    );
    assert_eq!(creates, 5827); // includes the ivm_term_sortkey dictionary table
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
fn c15_empty_frontier_agrees_with_dd() {
    let program: Program = serde_json::from_str(include_str!("corpus/8_c15_program.json")).unwrap();
    let sqlite_db = Connection::open_in_memory().unwrap();
    let dd_db = Connection::open_in_memory().unwrap();
    let mut sqlite_host = Raw::with_connection(&sqlite_db);
    let mut dd_host = Raw::with_connection(&dd_db);
    let mut sqlite = Sqlite::install(&program, &mut sqlite_host).unwrap();
    let mut dd = <Dd as Engine>::install(&program, &mut dd_host).unwrap();
    let frontier = Frontier { changes: vec![] };
    assert_eq!(sqlite.settle(frontier.clone(), &mut sqlite_host).unwrap().changes,
        dd.settle(frontier, &mut dd_host).unwrap().changes);
    for &rel in &program.outputs {
        assert_eq!(sqlite.snapshot(rel, &mut sqlite_host).unwrap(),
            dd.snapshot(rel, &mut dd_host).unwrap(), "output {rel}");
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

#[test]
#[ignore]
fn bench_materialization_boundaries() {
    let c15: Program = serde_json::from_str(include_str!("corpus/8_c15_program.json")).unwrap();
    let fixture = |text: &str| {
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        let program = serde_json::from_value(json["program"].clone()).unwrap();
        let dd_frontiers: Vec<Frontier> = serde_json::from_value(json["dd_frontiers"].clone()).unwrap();
        let texts: Vec<String> = serde_json::from_value(json["texts"].clone()).unwrap();
        let frontiers = random::drive::translated_frontiers::<Dd, Sqlite>(&program, &texts, &dd_frontiers).unwrap();
        (program, frontiers)
    };
    let count = fixture(include_str!("corpus/9_3_count_case.json"));
    let intern = fixture(include_str!("corpus/10_16_intern_row_reuse_case.json"));
    let cases = [
        ("c15", c15, vec![Frontier { changes: vec![] }]),
        ("3_count", count.0, count.1),
        ("16_intern_row_reuse", intern.0, intern.1),
    ];
    for (name, program, frontiers) in cases {
        let db = Connection::open_in_memory().unwrap();
        let mut host = Raw::with_connection(&db);
        let start = Instant::now();
        let mut engine = Sqlite::install(&program, &mut host).unwrap();
        let install_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        for frontier in frontiers {
            engine.settle(frontier, &mut host).unwrap();
        }
        let settle_ms = start.elapsed().as_secs_f64() * 1000.0;
        eprintln!("bench_ {name} install_ms={install_ms:.3} settle_ms={settle_ms:.3} total_ms={:.3}", install_ms + settle_ms);
    }
}

#[test]
#[ignore]
fn bench_settle_frontiers() {
    let program: Program = serde_json::from_str(include_str!("corpus/8_c15_program.json")).unwrap();
    let sources = program.rels.iter().filter(|r| r.kind == RelKind::Source && r.name.starts_with("__ir_seed_n"))
        .collect::<Vec<_>>();
    let sizes = [483, 515, 134, 102, 3097, 5, 5, 3, 1, 3];
    let mut ordinal = 0i64;
    let frontiers = sizes.map(|size| Frontier { changes: (0..size).map(|_| {
        let source = sources[ordinal as usize % sources.len()];
        ordinal += 1;
        SourceChange { rel: source.id, row: vec![ordinal; source.cols.len()], w: 1 }
    }).collect() });
    fn measure<E: Engine>(name: &str, program: &Program, frontiers: &[Frontier]) -> Vec<Vec<(u32, Vec<i64>, i64)>> {
        let db = Connection::open_in_memory().unwrap();
        let mut host = Raw::with_connection(&db);
        let start = Instant::now();
        let mut engine = E::install(program, &mut host).unwrap();
        eprintln!("settle_bench engine={name} install_ms={:.3}", start.elapsed().as_secs_f64() * 1000.0);
        frontiers.iter().enumerate().map(|(at, frontier)| {
            let start = Instant::now();
            let delta = engine.settle(frontier.clone(), &mut host).unwrap();
            eprintln!("settle_bench engine={name} frontier={at} inputs={} outputs={} statements={:?} settle_ms={:.3}",
                frontier.changes.len(), delta.changes.len(), engine.counters().statements,
                start.elapsed().as_secs_f64() * 1000.0);
            delta.changes
        }).collect()
    }
    let dd = measure::<Dd>("dd", &program, &frontiers);
    let sqlite = measure::<Sqlite>("sqlite", &program, &frontiers);
    assert_eq!(sqlite, dd);
}
