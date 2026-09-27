mod support;

use hafley_observe::{assert_growth_sized, Growth, SpanCounts};
use ivm_dd::{Counters, Dd, Engine, Frontier, Op, Program, Raw, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::{Cell as SqlCell, Program as SqlProgram, SourceChange as SqlChange, Sqlite};
use rusqlite::Connection;
use std::collections::BTreeMap;

fn sample(value: u64) -> SpanCounts {
    SpanCounts { entries: BTreeMap::from([("probe".into(), value as usize)]), ..SpanCounts::default() }
}

fn change(rel: u32, row: Vec<i64>) -> SourceChange {
    SourceChange { rel, row, w: 1 }
}

fn run<E: Engine>(program: &Program, changes: Vec<SourceChange>) -> Counters {
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = E::install(program, &mut host).unwrap();
    engine.settle(Frontier { changes }, &mut host).unwrap();
    engine.counters()
}

fn join_work<E: Engine>(loaded: usize) -> u64 {
    let program = support::program("0_access");
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = E::install(&program, &mut host).unwrap();
    let mut load: Vec<_> = (0..loaded).map(|i| change(0, vec![i as i64, i as i64])).collect();
    load.push(change(1, vec![100_000, 7]));
    engine.settle(Frontier { changes: load }, &mut host).unwrap();
    engine.settle(Frontier { changes: vec![change(0, vec![100_000, 100_000])] }, &mut host).unwrap();
    engine.counters().delta_rows.join.unwrap()
}

fn rounds<E: Engine>(depth: usize) -> u64 {
    let program = support::program("7_reach");
    let edges = (0..depth).map(|i| change(0, vec![i as i64, i as i64 + 1])).collect();
    run::<E>(&program, edges).rounds.unwrap()
}

fn mint_program() -> Program {
    Program {
        rels: vec![
            Relation { id: 0, name: "seed".into(), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "token".into(), cols: vec![Ty::Id, Ty::Int], kind: RelKind::Constructor },
            Relation { id: 2, name: "out".into(), cols: vec![Ty::Int, Ty::Int, Ty::Id], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::Mint { input: 0, functor: 1, args: vec![0] }],
        strata: vec![Stratum::Let { id: 2, body: 1 }],
        outputs: vec![2],
        texts: vec![],
    }
}

fn interned<E: Engine>(distinct: usize) -> u64 {
    let changes = (0..distinct).flat_map(|value| [
        change(0, vec![value as i64, 0]),
        change(0, vec![value as i64, 1]),
    ]).collect();
    run::<E>(&mint_program(), changes).interned.unwrap()
}

fn growth<E: Engine>() {
    let small = join_work::<E>(100);
    let large = join_work::<E>(1_000);
    assert_eq!((small, large), (1, 1));
    assert_growth_sized(&sample(small), &sample(large), "probe", 100, 1_000, Growth::Constant);

    let small = rounds::<E>(2);
    let large = rounds::<E>(40);
    assert_eq!((small, large), (2, 40));
    assert!(small > 0);
    assert_growth_sized(&sample(small), &sample(large), "probe", 2, 40, Growth::Linear);

    let counters = run::<E>(&mint_program(), vec![
        change(0, vec![1, 10]), change(0, vec![1, 20]), change(0, vec![2, 30]),
    ]);
    assert_eq!(counters.interned, Some(2));
    assert_eq!(counters.delta_rows.mint, Some(3));

    let small = interned::<E>(10);
    let large = interned::<E>(1_000);
    assert_eq!((small, large), (10, 1_000));
    assert_growth_sized(&sample(small), &sample(large), "probe", 10, 1_000, Growth::Linear);
}

#[test]
fn work_growth_both_engines() {
    growth::<Dd>();
    growth::<Sqlite>();
}

#[test]
fn sql_text_reports_boundary_work() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE seed(x INTEGER NOT NULL)").unwrap();
    let program = SqlProgram::install(&db, "work", "SELECT x FROM seed").unwrap();
    let (rows, counters) = program.settle_counted(&db, &[SqlChange::insert("seed", [SqlCell::Integer(7)])]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(counters.rows_written, 1);
    assert!(counters.statements.unwrap() > 0);
    assert_eq!(counters.delta_rows.join, None);
    assert_eq!(counters.rounds, None);
}
