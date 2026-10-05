//! Non-monotone LetRec limits, both engines: `17_nonmonotone_walk` with `limit = 8`. A walker from
//! `s` to the stop `t` changes `cur` in body evaluations 1 ..= t - s + 1, so distance 7 settles and
//! distance 8 reports `LetRecLimit(8)`.

use ivm_dd::Dd;
use ivm_engine::{Engine, EngineError, ErrorKind, Stage};
use ivm_ir::{Delta, Frontier, Program, SourceChange};
use ivm_sqlite::Sqlite;

const START: u32 = 0;
const STOP: u32 = 1;

fn program() -> Program {
    serde_json::from_str(include_str!("../../ivm-dd/oracle/17_nonmonotone_walk.program.json")).unwrap()
}

fn frontier(changes: &[(u32, i64, i64)]) -> Frontier {
    Frontier { changes: changes.iter().map(|(rel, n, w)| SourceChange { rel: *rel, row: vec![*n], w: *w }).collect() }
}

/// Every settle's result, then the snapshot of `cur` after the last settle that succeeded.
fn run<E: Engine>(steps: &[&[(u32, i64, i64)]]) -> (Vec<Result<Delta, EngineError>>, Vec<(Vec<i64>, i64)>) {
    let mut engine = E::install(&program()).unwrap();
    let mut results = Vec::new();
    for step in steps {
        let result = engine.settle(frontier(step));
        let failed = result.is_err();
        results.push(result);
        if failed { return (results, Vec::new()); }
    }
    let snapshot = engine.snapshot(2).unwrap();
    (results, snapshot)
}

fn both(steps: &[&[(u32, i64, i64)]]) -> String {
    let (dd, dd_cur) = run::<Dd>(steps);
    let (sqlite, sqlite_cur) = run::<Sqlite>(steps);
    assert_eq!(format!("{dd:?}"), format!("{sqlite:?}"), "dd and sqlite differ");
    assert_eq!(dd_cur, sqlite_cur, "dd and sqlite snapshots differ");
    format!("{dd:?}\ncur {dd_cur:?}")
}

fn limit_error() -> EngineError {
    EngineError::new(Stage::Settle, Some(2), ErrorKind::LetRecLimit(8))
}

#[test]
fn limit_not_hit_at_distance_seven() {
    let out = both(&[&[(START, 1, 1), (STOP, 8, 1)]]);
    assert_eq!(out, "[Ok(Delta { tick: 0, changes: [(2, [8], 1), (3, [1], 1)] })]\ncur [([8], 1)]");
}

#[test]
fn limit_hit_at_distance_eight() {
    let (dd, _) = run::<Dd>(&[&[(START, 1, 1), (STOP, 9, 1)]]);
    let (sqlite, _) = run::<Sqlite>(&[&[(START, 1, 1), (STOP, 9, 1)]]);
    assert_eq!(dd, vec![Err(limit_error())]);
    assert_eq!(sqlite, vec![Err(limit_error())]);
}

#[test]
fn limit_hit_by_a_later_frontier() {
    let steps: &[&[(u32, i64, i64)]] = &[&[(START, 1, 1), (STOP, 8, 1)], &[(STOP, 8, -1), (STOP, 9, 1)]];
    let (dd, _) = run::<Dd>(steps);
    let (sqlite, _) = run::<Sqlite>(steps);
    assert_eq!(dd[1], Err(limit_error()));
    assert_eq!(sqlite[1], Err(limit_error()));
    assert_eq!(format!("{:?}", dd[0]), format!("{:?}", sqlite[0]));
}

#[test]
fn diverging_walker_stops_at_the_limit() {
    // No stop at all: the walker never settles; the limit ends the loop.
    let (dd, _) = run::<Dd>(&[&[(START, 1, 1)]]);
    let (sqlite, _) = run::<Sqlite>(&[&[(START, 1, 1)]]);
    assert_eq!(dd, vec![Err(limit_error())]);
    assert_eq!(sqlite, vec![Err(limit_error())]);
}

#[test]
fn sqlite_rolls_back_a_limit_hit() {
    let mut engine = Sqlite::install(&program()).unwrap();
    engine.settle(frontier(&[(START, 1, 1), (STOP, 8, 1)])).unwrap();
    assert_eq!(engine.settle(frontier(&[(STOP, 8, -1), (STOP, 9, 1)])), Err(limit_error()));
    assert_eq!(engine.snapshot(2).unwrap(), vec![(vec![8], 1)]);
    let back = engine.settle(frontier(&[(STOP, 8, -1), (STOP, 7, 1)])).unwrap();
    assert_eq!(back.changes, vec![(2, vec![7], 1), (2, vec![8], -1)]);
}
