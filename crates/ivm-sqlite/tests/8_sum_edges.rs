use ivm_dd::Dd;
use ivm_engine::Engine;
use ivm_ir::{Agg, AnyValue, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use rusqlite::{types::Value, Connection};

#[path = "value_edges/0_support.rs"]
mod support;
use support::sqlite_rows;

fn sum_ir() -> Program {
    Program {
        terms: vec![],
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "src".into(), cols: vec![Ty::Int, Ty::Int, Ty::Any], kind: RelKind::Source },
            Relation { id: 1, name: "sums".into(), cols: vec![Ty::Int, Ty::Any, Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::Reduce { input: 0, key: vec![1], aggs: vec![Agg::Sum(2), Agg::Count] }],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    }
}

fn check_sum<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(id INTEGER PRIMARY KEY, g INTEGER, v);").unwrap();
    let mut engine = E::install(&sum_ir()).unwrap();
    for (id, value, literal, sign) in [
        (1, AnyValue::Integer(5), "5", 1),
        (2, AnyValue::Integer(7), "7", 1),
        (3, AnyValue::Real(0.5f64.to_bits()), "0.5", 1),
        (3, AnyValue::Real(0.5f64.to_bits()), "0.5", -1),
        (4, AnyValue::Null, "NULL", 1),
        (1, AnyValue::Integer(5), "5", -1),
        (2, AnyValue::Integer(7), "7", -1),
        (5, AnyValue::Text("8".into()), "'8'", 1),
        (6, AnyValue::Blob(vec![b'1']), "x'31'", 1),
        (6, AnyValue::Blob(vec![b'1']), "x'31'", -1),
    ] {
        let sql = if sign > 0 { format!("INSERT INTO src VALUES ({id}, 9, {literal})") }
            else { format!("DELETE FROM src WHERE id={id}") };
        oracle.execute_batch(&sql).unwrap();
        let encoded = engine.intern_any(&value).unwrap();
        engine.settle(Frontier { changes: vec![SourceChange { rel: 0, row: vec![id, 9, encoded], w: sign }] }).unwrap();
        let want = sqlite_rows(&oracle, "SELECT g, sum(v), count(*) FROM src GROUP BY g");
        let mut got = engine.snapshot(1).unwrap().into_iter().map(|(row, _)| {
            let sum = match engine.any_value(row[1]).unwrap() {
                AnyValue::Null => Value::Null,
                AnyValue::Integer(v) => Value::Integer(v),
                AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
                value => panic!("sum class {value:?}"),
            };
            vec![Value::Integer(row[0]), sum, Value::Integer(row[2])]
        }).collect::<Vec<_>>();
        got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        assert_eq!(got, want, "after source row {id} sign {sign}");
    }
}

#[test]
fn dynamic_any_sum_matches_sqlite() {
    check_sum::<Dd>();
    check_sum::<Sqlite>();
}

fn check_overflow<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(v); INSERT INTO src VALUES (9223372036854775807),(1);").unwrap();
    let oracle_error = oracle.query_row::<i64, _, _>("SELECT sum(v) FROM src", [], |r| r.get(0)).unwrap_err().to_string();
    assert!(oracle_error.contains("integer overflow"));
    let total: f64 = oracle.query_row("SELECT total(v) FROM src", [], |r| r.get(0)).unwrap();
    assert!(total.is_finite());

    let mut engine = E::install(&sum_ir()).unwrap();
    let changes = [(1, i64::MAX), (2, 1)].into_iter().map(|(id, v)| SourceChange {
        rel: 0, row: vec![id, 9, engine.intern_any(&AnyValue::Integer(v)).unwrap()], w: 1,
    }).collect();
    let error = engine.settle(Frontier { changes }).unwrap_err().to_string();
    assert!(error.contains("integer overflow"), "{error}");
}

#[test]
fn any_sum_integer_overflow_matches_sqlite() {
    check_overflow::<Dd>();
    check_overflow::<Sqlite>();
}

fn check_overflow_cleared<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(id INTEGER, g INTEGER, v); INSERT INTO src VALUES (1,9,9223372036854775807),(2,9,1),(3,9,0.0);").unwrap();
    let want = sqlite_rows(&oracle, "SELECT g,sum(v),count(*) FROM src GROUP BY g");
    let mut engine = E::install(&sum_ir()).unwrap();
    let changes = [(1, AnyValue::Integer(i64::MAX)), (2, AnyValue::Integer(1)), (3, AnyValue::Real(0.0f64.to_bits()))]
        .iter().map(|(id, value)| SourceChange { rel: 0, row: vec![*id, 9, engine.intern_any(value).unwrap()], w: 1 }).collect();
    engine.settle(Frontier { changes }).unwrap();
    let row = &engine.snapshot(1).unwrap()[0].0;
    let sum = match engine.any_value(row[1]).unwrap() {
        AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
        value => panic!("sum class {value:?}"),
    };
    assert_eq!(vec![vec![Value::Integer(row[0]), sum, Value::Integer(row[2])]], want);
}

#[test]
fn real_after_integer_overflow_matches_sqlite() {
    check_overflow_cleared::<Dd>();
    check_overflow_cleared::<Sqlite>();
}
