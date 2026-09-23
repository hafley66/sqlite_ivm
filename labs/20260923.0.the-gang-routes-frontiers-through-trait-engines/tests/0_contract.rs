use lab_20260923_0::{
    Change, EngineError, FrontierEngine, Plan, RustEngine, SourceRow, SqliteEngine, WeightedRow,
};
use std::collections::BTreeMap;

fn change(source: u8, id: i64, cells: &[i64], weight: i8) -> Change {
    Change {
        source,
        row: SourceRow {
            id,
            cells: cells.to_vec(),
        },
        weight,
    }
}

fn snapshots(text: &str, support_column: bool) -> BTreeMap<String, Vec<Vec<i64>>> {
    let mut out = BTreeMap::<String, Vec<Vec<i64>>>::new();
    for line in text.lines() {
        let cells = line.split('\t').collect::<Vec<_>>();
        let end = if support_column {
            cells.len() - 1
        } else {
            cells.len()
        };
        let row = cells[1..end].iter().map(|v| v.parse().unwrap()).collect();
        out.entry(cells[0].to_string()).or_default().push(row);
    }
    out
}

fn deltas(text: &str) -> BTreeMap<String, Vec<WeightedRow>> {
    let mut out = BTreeMap::<String, Vec<WeightedRow>>::new();
    for line in text.lines() {
        let cells = line.split('\t').collect::<Vec<_>>();
        let changes = out.entry(cells[0].to_string()).or_default();
        if cells[1] == "EMPTY" {
            continue;
        }
        changes.push(WeightedRow {
            cells: cells[1..cells.len() - 1]
                .iter()
                .map(|v| v.parse().unwrap())
                .collect(),
            weight: cells.last().unwrap().parse().unwrap(),
        });
    }
    out
}

fn check<
    E: FrontierEngine<Plan = Plan, Change = Change, Output = Vec<i64>, Error = EngineError>,
>(
    engine: &mut E,
    name: &str,
    batch: &[Change],
    expected: &BTreeMap<String, Vec<Vec<i64>>>,
    diffs: &BTreeMap<String, Vec<WeightedRow>>,
) {
    let mut actual = engine.apply(batch).unwrap().changes;
    let mut want = diffs[name].clone();
    actual.sort_by(|a, b| (&a.cells, a.weight).cmp(&(&b.cells, b.weight)));
    want.sort_by(|a, b| (&a.cells, a.weight).cmp(&(&b.cells, b.weight)));
    assert_eq!(actual, want, "delta at {name}");
    assert_eq!(
        engine.snapshot().unwrap(),
        expected.get(name).cloned().unwrap_or_default(),
        "snapshot at {name}"
    );
}

fn access_graph_matches_oracle_at_every_frontier<
    E: FrontierEngine<Plan = Plan, Change = Change, Output = Vec<i64>, Error = EngineError>,
>(
    mut engine: E,
) {
    let expected = snapshots(
        include_str!("../../../plans/engine-iso/3_expected.tsv"),
        true,
    );
    let diffs = deltas(include_str!("../../../plans/engine-iso/3a_deltas.tsv"));
    engine
        .install(Plan::JoinUnion {
            left: 0,
            right: 1,
            direct: 2,
            left_key: 1,
            right_key: 0,
            left_output: 0,
            right_output: 1,
            direct_output: [0, 1],
        })
        .unwrap();
    check(
        &mut engine,
        "0_initial",
        &[
            change(0, 1, &[1, 10], 1),
            change(0, 2, &[1, 20], 1),
            change(1, 1, &[10, 100], 1),
            change(1, 2, &[20, 100], 1),
            change(2, 1, &[3, 300], 1),
        ],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "1_both_join_inputs",
        &[change(0, 3, &[2, 10], 1), change(1, 3, &[10, 200], 1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "2_duplicate_union_support",
        &[change(2, 2, &[1, 200], 1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "3_join_support_retract",
        &[change(0, 1, &[1, 10], -1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "4_last_join_support",
        &[change(1, 2, &[20, 100], -1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "5_last_union_support",
        &[change(2, 2, &[1, 200], -1)],
        &expected,
        &diffs,
    );
    check(&mut engine, "6_savepoint_rollback", &[], &expected, &diffs);
    check(
        &mut engine,
        "7_transaction_rollback",
        &[],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "8_update",
        &[change(1, 3, &[10, 200], -1), change(1, 3, &[10, 300], 1)],
        &expected,
        &diffs,
    );
    let before = engine.snapshot().unwrap();
    let err = engine.apply(&[change(0, 99, &[9, 9], -1)]).unwrap_err();
    assert!(!err.message.is_empty());
    assert_eq!(engine.snapshot().unwrap(), before);
    engine.teardown().unwrap();
}

fn grouped_count_sum_matches_oracle_at_every_frontier<
    E: FrontierEngine<Plan = Plan, Change = Change, Output = Vec<i64>, Error = EngineError>,
>(
    mut engine: E,
) {
    let expected = snapshots(
        include_str!("../../../plans/engine-iso/3c_aggregate_expected.tsv"),
        false,
    );
    let diffs = deltas(include_str!(
        "../../../plans/engine-iso/3d_aggregate_deltas.tsv"
    ));
    engine
        .install(Plan::GroupCountSum {
            source: 0,
            group: 0,
            value: 1,
        })
        .unwrap();
    check(
        &mut engine,
        "0_initial",
        &[
            change(0, 1, &[10, 5], 1),
            change(0, 2, &[10, 7], 1),
            change(0, 3, &[20, 11], 1),
        ],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "1_move_and_add",
        &[
            change(0, 4, &[10, 3], 1),
            change(0, 3, &[20, 11], -1),
            change(0, 3, &[10, 11], 1),
        ],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "2_cross_zero",
        &[change(0, 2, &[10, 7], -1), change(0, 2, &[10, -7], 1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "3_delete_two",
        &[change(0, 1, &[10, 5], -1), change(0, 4, &[10, 3], -1)],
        &expected,
        &diffs,
    );
    check(
        &mut engine,
        "4_empty",
        &[change(0, 2, &[10, -7], -1), change(0, 3, &[10, 11], -1)],
        &expected,
        &diffs,
    );
    check(&mut engine, "5_rollback", &[], &expected, &diffs);
    assert!(engine
        .apply(&[change(0, 5, &[30, 9], 1), change(0, 5, &[30, 9], -1)])
        .unwrap()
        .changes
        .is_empty());
    assert!(engine.snapshot().unwrap().is_empty());
}

#[test]
fn rust_access() {
    access_graph_matches_oracle_at_every_frontier(RustEngine::new());
}

#[test]
fn rust_group() {
    grouped_count_sum_matches_oracle_at_every_frontier(RustEngine::new());
}

#[test]
fn sqlite_access() {
    access_graph_matches_oracle_at_every_frontier(SqliteEngine::memory().unwrap());
}

#[test]
fn sqlite_group() {
    grouped_count_sum_matches_oracle_at_every_frontier(SqliteEngine::memory().unwrap());
}
