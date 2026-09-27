use ivm_dd::Dd;
use ivm_engine::{Engine, Raw};
use ivm_ir::{AnyValue, Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::Sqlite;
use ivm_sqlite::Frontier as _;
use rusqlite::{types::Value, Connection};

#[path = "value_edges/0_support.rs"]
mod support;
use support::sqlite_rows;

fn blob_ir() -> Program {
    Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "src".into(), cols: vec![Ty::Any, Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "ordered".into(), cols: vec![Ty::Any, Ty::Int], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::TopK { input: 0, key: vec![], order: vec![ivm_ir::Order { col: 0, desc: false }], limit: 4 }],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    }
}

fn check_blob<E: Engine>() {
    let oracle = Connection::open_in_memory().unwrap();
    oracle.execute_batch("CREATE TABLE src(k, n INTEGER); INSERT INTO src VALUES (x'01',1),('z',2),(x'00ff',3),(4,4),(x'00',5);").unwrap();
    let want = sqlite_rows(&oracle, "SELECT k,n FROM src ORDER BY k,n LIMIT 4");
    let db = Connection::open_in_memory().unwrap();
    let mut host = Raw::with_connection(&db);
    let mut engine = E::install(&blob_ir(), &mut host).unwrap();
    let values = [AnyValue::Blob(vec![1]), AnyValue::Text("z".into()), AnyValue::Blob(vec![0, 255]), AnyValue::Integer(4), AnyValue::Blob(vec![0])];
    let changes = values.iter().enumerate().map(|(i, v)| SourceChange {
        rel: 0, row: vec![engine.intern_any(v, &mut host).unwrap(), i as i64 + 1], w: 1,
    }).collect();
    engine.settle(Frontier { changes }, &mut host).unwrap();
    let mut got = engine.snapshot(1, &mut host).unwrap().into_iter().map(|(row, _)| {
        let value = match engine.any_value(row[0], &mut host).unwrap() {
            AnyValue::Null => Value::Null,
            AnyValue::Integer(v) => Value::Integer(v),
            AnyValue::Real(bits) => Value::Real(f64::from_bits(bits)),
            AnyValue::Text(v) => Value::Text(v),
            AnyValue::Blob(v) => Value::Blob(v),
        };
        vec![value, Value::Integer(row[1])]
    }).collect::<Vec<_>>();
    got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(got, want);
}

#[test]
fn blob_order_follows_sqlite_storage_classes() {
    check_blob::<Dd>();
    check_blob::<Sqlite>();
}

#[test]
fn blob_dictionary_ids_survive_program_reopen() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE blobs(k); INSERT INTO blobs VALUES (x'00ff'),(x'01');").unwrap();
    let program = ivm_sqlite::Program::install(&db, "blob_reopen", "SELECT k FROM blobs").unwrap();
    let first: i64 = db.query_row("SELECT id FROM ivm_blob_dict WHERE bytes=x'00ff'", [], |r| r.get(0)).unwrap();
    drop(program);
    let program = ivm_sqlite::Program::open(&db, "blob_reopen").unwrap();
    db.execute_batch("INSERT INTO blobs VALUES (x'00ff'),(x'ff');").unwrap();
    let again: i64 = db.query_row("SELECT id FROM ivm_blob_dict WHERE bytes=x'00ff'", [], |r| r.get(0)).unwrap();
    let later: i64 = db.query_row("SELECT id FROM ivm_blob_dict WHERE bytes=x'ff'", [], |r| r.get(0)).unwrap();
    assert_eq!(again, first);
    assert!(later > first);
    let mut got = program.snapshot(&db).unwrap().into_iter().map(|row| row.0).collect::<Vec<_>>();
    got.sort_by_key(|row| format!("{row:?}"));
    let want = sqlite_rows(&db, "SELECT DISTINCT k FROM blobs").into_iter().map(|row| row.into_iter().map(|value| match value {
        Value::Blob(v) => ivm_sqlite::Cell::Blob(v),
        _ => unreachable!(),
    }).collect::<Vec<_>>()).collect::<Vec<_>>();
    assert_eq!(got, want);
    assert_eq!(got, vec![
        vec![ivm_sqlite::Cell::Blob(vec![0, 255])],
        vec![ivm_sqlite::Cell::Blob(vec![1])],
        vec![ivm_sqlite::Cell::Blob(vec![255])],
    ]);
    program.teardown(&db).unwrap();
}
