use ivm_engine::{Engine, Raw};
use ivm_ir::{Frontier, SourceChange};
use ivm_sqlite::Sqlite;
use rusqlite::Connection;

fn frontier(changes: &[(u32, &[i64], i64)]) -> Frontier {
    Frontier {
        changes: changes
            .iter()
            .map(|(rel, row, w)| SourceChange {
                rel: *rel,
                row: row.to_vec(),
                w: *w,
            })
            .collect(),
    }
}

#[test]
fn host_owned_connection_runs_typed_access() {
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let ir =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/0_access.program.json")).unwrap();
    let mut engine = Sqlite::install(&ir, &mut host).unwrap();
    let first = engine
        .settle(frontier(&[(0, &[1, 10], 1), (1, &[10, 100], 1)]), &mut host)
        .unwrap();
    assert_eq!(first.changes, vec![(3, vec![1, 100], 1)]);
    assert_eq!(
        engine.snapshot(3, &mut host).unwrap(),
        vec![(vec![1, 100], 1)]
    );
    let second = engine
        .settle(
            frontier(&[(2, &[1, 100], 1), (1, &[10, 100], -1)]),
            &mut host,
        )
        .unwrap();
    assert!(second.changes.is_empty());
    assert_eq!(
        engine.snapshot(3, &mut host).unwrap(),
        vec![(vec![1, 100], 1)]
    );
}

#[test]
fn host_owned_connection_runs_typed_topk_weights() {
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut ir: ivm_ir::Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/6_topk.program.json")).unwrap();
    ir.strata.truncate(1);
    ir.outputs.truncate(1);
    let mut engine = Sqlite::install(&ir, &mut host).unwrap();
    let first = engine
        .settle(
            frontier(&[(0, &[1, 10, 5], 1), (0, &[2, 10, 7], 1)]),
            &mut host,
        )
        .unwrap();
    assert_eq!(first.changes, vec![(1, vec![2, 10, 7], 1)]);
    assert_eq!(
        engine.snapshot(1, &mut host).unwrap(),
        vec![(vec![2, 10, 7], 1)]
    );
    let second = engine
        .settle(frontier(&[(0, &[2, 10, 7], -1)]), &mut host)
        .unwrap();
    assert_eq!(
        second.changes,
        vec![(1, vec![1, 10, 5], 1), (1, vec![2, 10, 7], -1)]
    );
}
