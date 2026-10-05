//! Nested LetRec, both engines: `18_nested_wave_reach` (outer LetRec `[started, g]` with
//! `limit = 8`, nested `[reach]`). The inner limit applies per outer round; the outer limit to
//! the waves.

use ivm_dd::Dd;
use ivm_engine::{Engine, EngineError, ErrorKind, Stage};
use ivm_ir::{Delta, Frontier, Program, SourceChange, Stratum};
use ivm_sqlite::Sqlite;

const EDGE: u32 = 0;
const ROOT: u32 = 1;
const CUT: u32 = 2;

/// The script program with the outer and the nested limit replaced.
fn program(outer: Option<u32>, inner: Option<u32>) -> Program {
    let mut program: Program = serde_json::from_str(include_str!("../../ivm-dd/oracle/18_nested_wave_reach.program.json")).unwrap();
    let Stratum::LetRec(rec) = &mut program.strata[0] else { panic!("one LetRec") };
    rec.limit = outer;
    rec.nested[0].limit = inner;
    program
}

fn frontier(changes: &[(u32, &[i64], i64)]) -> Frontier {
    Frontier { changes: changes.iter().map(|(rel, row, w)| SourceChange { rel: *rel, row: row.to_vec(), w: *w }).collect() }
}

type Steps<'a> = &'a [&'a [(u32, &'a [i64], i64)]];

/// Every settle's result up to the first error, then the snapshot of `g` if none failed.
fn run<E: Engine>(program: &Program, steps: Steps) -> (Vec<Result<Delta, EngineError>>, Vec<(Vec<i64>, i64)>) {
    let mut engine = E::install(program).unwrap();
    let mut results = Vec::new();
    for step in steps {
        let result = engine.settle(frontier(step));
        let failed = result.is_err();
        results.push(result);
        if failed { return (results, Vec::new()); }
    }
    (results, engine.snapshot(4).unwrap())
}

fn both(program: &Program, steps: Steps) -> String {
    let (dd, dd_g) = run::<Dd>(program, steps);
    let (sqlite, sqlite_g) = run::<Sqlite>(program, steps);
    assert_eq!(format!("{dd:?}"), format!("{sqlite:?}"), "dd and sqlite differ");
    assert_eq!(dd_g, sqlite_g, "dd and sqlite snapshots differ");
    format!("{dd:?}\ng {dd_g:?}")
}

const CHAIN: &[(u32, &[i64], i64)] = &[(EDGE, &[1, 2], 1), (EDGE, &[2, 3], 1), (EDGE, &[3, 4], 1), (ROOT, &[1], 1)];

fn limit(rel: u32, limit: u32) -> EngineError {
    EngineError::new(Stage::Settle, Some(rel), ErrorKind::LetRecLimit(limit))
}

#[test]
fn inner_recompute_path_matches_inner_dred() {
    // A nested limit puts the inner LetRec on the sqlite recompute path; no limit keeps DRed.
    let steps: Steps = &[CHAIN, &[(CUT, &[2], 1)], &[(CUT, &[2], -1), (CUT, &[3], 1)], &[(EDGE, &[1, 2], -1)]];
    let dred = both(&program(Some(8), None), steps);
    let recompute = both(&program(Some(8), Some(8)), steps);
    assert_eq!(dred, recompute);
    assert_eq!(dred, "[Ok(Delta { tick: 0, changes: [(4, [1, 2], 1), (4, [2, 3], 1), (4, [3, 4], 1)] }), \
        Ok(Delta { tick: 1, changes: [(4, [2, 3], -1)] }), \
        Ok(Delta { tick: 2, changes: [(4, [2, 3], 1), (4, [3, 4], -1)] }), \
        Ok(Delta { tick: 3, changes: [(4, [1, 2], -1), (4, [3, 4], 1)] })]\n\
        g [([2, 3], 1), ([3, 4], 1)]");
}

#[test]
fn inner_limit_not_hit() {
    // reach changes in evaluations 1..=4 over the chain 1 -> 2 -> 3 -> 4.
    let out = both(&program(Some(8), Some(4)), &[CHAIN]);
    assert_eq!(out, "[Ok(Delta { tick: 0, changes: [(4, [1, 2], 1), (4, [2, 3], 1), (4, [3, 4], 1)] })]\n\
        g [([1, 2], 1), ([2, 3], 1), ([3, 4], 1)]");
}

#[test]
fn inner_limit_hit() {
    let program = program(Some(8), Some(3));
    let (dd, _) = run::<Dd>(&program, &[CHAIN]);
    let (sqlite, _) = run::<Sqlite>(&program, &[CHAIN]);
    assert_eq!(dd, vec![Err(limit(5, 3))]);
    assert_eq!(sqlite, vec![Err(limit(5, 3))]);
}

#[test]
fn inner_limit_hit_by_a_later_frontier() {
    let program = program(Some(8), Some(4));
    let steps: Steps = &[CHAIN, &[(EDGE, &[4, 5], 1)]];
    let (dd, _) = run::<Dd>(&program, steps);
    let (sqlite, _) = run::<Sqlite>(&program, steps);
    assert_eq!(format!("{:?}", dd[0]), format!("{:?}", sqlite[0]));
    assert_eq!(dd[1], Err(limit(5, 4)));
    assert_eq!(sqlite[1], Err(limit(5, 4)));
}

#[test]
fn outer_limit_not_hit() {
    // No reachable cut: the second evaluation repeats the first.
    let out = both(&program(Some(1), None), &[CHAIN, &[(CUT, &[7], 1)]]);
    assert_eq!(out, "[Ok(Delta { tick: 0, changes: [(4, [1, 2], 1), (4, [2, 3], 1), (4, [3, 4], 1)] }), \
        Ok(Delta { tick: 1, changes: [] })]\n\
        g [([1, 2], 1), ([2, 3], 1), ([3, 4], 1)]");
}

#[test]
fn outer_limit_hit() {
    // A reachable cut rewrites the graph in the second evaluation.
    let program = program(Some(1), None);
    let steps: Steps = &[CHAIN, &[(CUT, &[2], 1)]];
    let (dd, _) = run::<Dd>(&program, steps);
    let (sqlite, _) = run::<Sqlite>(&program, steps);
    assert_eq!(format!("{:?}", dd[0]), format!("{:?}", sqlite[0]));
    assert_eq!(dd[1], Err(limit(3, 1)));
    assert_eq!(sqlite[1], Err(limit(3, 1)));
}

#[test]
fn two_deep_is_refused() {
    let mut program = program(Some(8), None);
    let Stratum::LetRec(rec) = &mut program.strata[0] else { panic!("one LetRec") };
    let inner = rec.nested[0].clone();
    rec.nested[0].nested.push(inner);
    let unsupported = |e: EngineError| e.kind;
    assert_eq!(Dd::install(&program).map(drop).map_err(unsupported), Err(ErrorKind::Unsupported("LetRec nested two deep")));
    assert_eq!(Sqlite::install(&program).map(drop).map_err(unsupported), Err(ErrorKind::Unsupported("LetRec nested two deep")));
}

#[test]
fn sqlite_rolls_back_an_inner_limit_hit() {
    let mut engine = Sqlite::install(&program(Some(8), Some(4))).unwrap();
    engine.settle(frontier(CHAIN)).unwrap();
    assert_eq!(engine.settle(frontier(&[(EDGE, &[4, 5], 1)])), Err(limit(5, 4)));
    assert_eq!(engine.snapshot(4).unwrap(), vec![(vec![1, 2], 1), (vec![2, 3], 1), (vec![3, 4], 1)]);
    let back = engine.settle(frontier(&[(CUT, &[3], 1)])).unwrap();
    assert_eq!(back.changes, vec![(4, vec![3, 4], -1)]);
}

/// Work (`Sqlite::install_counted`) of the install and of the four settles of
/// `inner_recompute_path_matches_inner_dred`, inner DRed then inner recompute.
#[cfg(not(feature = "image"))]
#[test]
fn work_counts() {
    let steps: Steps = &[CHAIN, &[(CUT, &[2], 1)], &[(CUT, &[2], -1), (CUT, &[3], 1)], &[(EDGE, &[1, 2], -1)]];
    let mut lines = Vec::new();
    for (name, inner) in [("dred", None), ("recompute", Some(8))] {
        let mut engine = Sqlite::install_counted(&program(Some(8), inner)).unwrap();
        let install = engine.work().unwrap();
        for step in steps {
            engine.settle(frontier(step)).unwrap();
        }
        let settles = engine.work().unwrap().since(&install);
        lines.push(format!("{name} install {install:?}"));
        lines.push(format!("{name} settles {settles:?}"));
    }
    assert_eq!(lines.join("\n"), concat!(
        "dred install Work { creates: 64, schema_rows_parsed: 395, prepared: 85, prepared_bytes: 9246, unfolded_bytes: 9246, executions: 85, zero_row_executions: 1, vm_steps: 2216, fullscan_steps: 10, sorts: 1, autoindexes: 0, reprepares: 0, rows_returned: 20, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 3, written_term: 1 }\n",
        "dred settles Work { creates: 0, schema_rows_parsed: 0, prepared: 159, prepared_bytes: 20374, unfolded_bytes: 19966, executions: 841, zero_row_executions: 241, vm_steps: 43486, fullscan_steps: 414, sorts: 423, autoindexes: 0, reprepares: 0, rows_returned: 57, written_d: 346, written_i: 267, written_x: 0, written_scratch: 173, written_source: 8, written_catalog: 4, written_term: 0 }\n",
        "recompute install Work { creates: 64, schema_rows_parsed: 395, prepared: 85, prepared_bytes: 9243, unfolded_bytes: 9243, executions: 85, zero_row_executions: 1, vm_steps: 2216, fullscan_steps: 10, sorts: 1, autoindexes: 0, reprepares: 0, rows_returned: 20, written_d: 0, written_i: 0, written_x: 0, written_scratch: 0, written_source: 0, written_catalog: 3, written_term: 1 }\n",
        "recompute settles Work { creates: 0, schema_rows_parsed: 0, prepared: 149, prepared_bytes: 19688, unfolded_bytes: 19280, executions: 1042, zero_row_executions: 328, vm_steps: 49729, fullscan_steps: 469, sorts: 483, autoindexes: 0, reprepares: 0, rows_returned: 47, written_d: 420, written_i: 347, written_x: 0, written_scratch: 215, written_source: 8, written_catalog: 4, written_term: 0 }",
    ));
}
