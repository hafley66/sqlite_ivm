use ivm_dd::Dd;
use ivm_engine::{Engine, Raw};
use ivm_ir::{Agg, AnyValue, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use ivm_sqlite::Frontier as _;
use rusqlite::{types::Value, Connection};

#[path = "value_edges/0_support.rs"]
mod support;
use support::sqlite_rows;

#[test]
fn nullable_sql_text_source_matches_sqlite() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE nullable(k, v INTEGER); INSERT INTO nullable VALUES (NULL,1),(NULL,2),(5,NULL),(5,3);").unwrap();
    let query = "SELECT k, count(*) AS n, sum(v) AS total FROM nullable GROUP BY k";
    let want = sqlite_rows(&db, query);
    let program = ivm_sqlite::Program::install(&db, "nullable_edges", query).unwrap();
    let mut got = program.snapshot(&db).unwrap().into_iter().map(|ivm_sqlite::Tuple(row)| row.into_iter().map(|cell| match cell {
        ivm_sqlite::Cell::Null => Value::Null,
        ivm_sqlite::Cell::Integer(v) => Value::Integer(v),
        ivm_sqlite::Cell::Real(v) => Value::Real(v),
        ivm_sqlite::Cell::Text(v) => Value::Text(v),
        ivm_sqlite::Cell::Blob(v) => Value::Blob(v),
    }).collect::<Vec<_>>()).collect::<Vec<_>>();
    got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(got, want);
    program.teardown(&db).unwrap();
}

fn extrema_ir() -> Program {
    Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "src".into(), cols: vec![Ty::Int, Ty::Any], kind: RelKind::Source },
            Relation { id: 1, name: "bounds".into(), cols: vec![Ty::Int, Ty::Any, Ty::Any], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::Reduce { input: 0, key: vec![0], aggs: vec![Agg::Min(1), Agg::Max(1)] }],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    }
}

fn check_null_extrema<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(g INTEGER, v); INSERT INTO src VALUES (9,NULL),(9,5),(9,'z'),(9,x'01'),(8,NULL);").unwrap();
    let want = sqlite_rows(&oracle, "SELECT g,min(v),max(v) FROM src GROUP BY g");
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = E::install(&extrema_ir(), &mut host).unwrap();
    let changes = [(9, AnyValue::Null), (9, AnyValue::Integer(5)), (9, AnyValue::Text("z".into())), (9, AnyValue::Blob(vec![1])), (8, AnyValue::Null)]
        .iter().map(|(group, value)| SourceChange { rel: 0, row: vec![*group, engine.intern_any(value, &mut host).unwrap()], w: 1 }).collect();
    engine.settle(Frontier { changes }, &mut host).unwrap();
    let mut got = engine.snapshot(1, &mut host).unwrap().into_iter().map(|(row, _)| {
        let mut value = |id| match engine.any_value(id, &mut host).unwrap() {
            AnyValue::Null => Value::Null,
            AnyValue::Integer(v) => Value::Integer(v),
            AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
            AnyValue::Text(v) => Value::Text(v),
            AnyValue::Blob(v) => Value::Blob(v),
        };
        vec![Value::Integer(row[0]), value(row[1]), value(row[2])]
    }).collect::<Vec<_>>();
    got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(got, want);
}

#[test]
fn nulls_are_skipped_by_extrema() {
    check_null_extrema::<Dd>();
    check_null_extrema::<Sqlite>();
}
