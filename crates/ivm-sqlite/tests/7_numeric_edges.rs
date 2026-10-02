use ivm_dd::Dd;
use ivm_engine::Engine;
use ivm_ir::{Agg, AnyValue, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use rusqlite::{types::Value, Connection};

#[path = "value_edges/0_support.rs"]
mod support;
use support::sqlite_rows;

fn ir() -> Program {
    Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "src".into(), cols: vec![Ty::Any, Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "groups".into(), cols: vec![Ty::Any, Ty::Int], kind: RelKind::Derived },
            Relation { id: 2, name: "matches".into(), cols: vec![Ty::Any, Ty::Int, Ty::Any, Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Reduce { input: 0, key: vec![0], aggs: vec![Agg::Count] },
            Op::Join { inputs: vec![0, 0], equivalences: vec![vec![(0, 0), (1, 0)]] },
        ],
        strata: vec![Stratum::Let { id: 1, body: 1 }, Stratum::Let { id: 2, body: 2 }],
        outputs: vec![1, 2],
    }
}

fn check_numeric_equivalence<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(k, n INTEGER); INSERT INTO src VALUES (5, 1), (5.0, 2), ('5', 3), (6, 4), (NULL, 5), (NULL, 6);").unwrap();
    let want_groups = sqlite_rows(&oracle, "SELECT k, count(*) FROM src GROUP BY k");
    let want_joins = sqlite_rows(&oracle, "SELECT a.k, a.n, b.k, b.n FROM src a JOIN src b ON a.k = b.k");
    let mut engine = E::install(&ir()).unwrap();
    let values = [AnyValue::Integer(5), AnyValue::Real(5.0f64.to_bits()), AnyValue::Text("5".into()), AnyValue::Integer(6), AnyValue::Null, AnyValue::Null];
    let changes = values.iter().enumerate().map(|(i, value)| SourceChange {
        rel: 0,
        row: vec![engine.intern_any(value).unwrap(), i as i64 + 1],
        w: 1,
    }).collect();
    engine.settle(Frontier { changes }).unwrap();
    assert_rows(&engine, want_groups, want_joins);
    oracle.execute_batch("DELETE FROM src WHERE typeof(k)='integer' AND k=5").unwrap();
    let id = engine.intern_any(&AnyValue::Integer(5)).unwrap();
    engine.settle(Frontier { changes: vec![SourceChange { rel: 0, row: vec![id, 1], w: -1 }] }).unwrap();
    assert_rows(&engine,
        sqlite_rows(&oracle, "SELECT k, count(*) FROM src GROUP BY k"),
        sqlite_rows(&oracle, "SELECT a.k, a.n, b.k, b.n FROM src a JOIN src b ON a.k = b.k"));
}

fn assert_rows<E: Engine>(engine: &E, groups: Vec<Vec<Value>>, joins: Vec<Vec<Value>>) {
    for (rel, want, classes) in [(1, groups, vec![0]), (2, joins, vec![0, 2])] {
        let mut got = engine.snapshot(rel).unwrap().into_iter().flat_map(|(row, w)| {
            let values = row.into_iter().enumerate().map(|(i, cell)| {
                if classes.contains(&i) {
                    match engine.any_value(cell).unwrap() {
                        AnyValue::Null => Value::Null,
                        AnyValue::Integer(v) => Value::Integer(v),
                        AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
                        AnyValue::Text(v) => Value::Text(v),
                        AnyValue::Blob(v) => Value::Blob(v),
                    }
                } else { Value::Integer(cell) }
            }).collect::<Vec<_>>();
            std::iter::repeat_n(values, w as usize)
        }).collect::<Vec<_>>();
        got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        assert_eq!(got, want, "relation {rel}");
    }
}

#[test]
fn integer_real_any_group_and_join_match_sqlite() {
    check_numeric_equivalence::<Dd>();
    check_numeric_equivalence::<Sqlite>();
}
