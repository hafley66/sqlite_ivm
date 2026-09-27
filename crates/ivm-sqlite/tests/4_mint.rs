use ivm_engine::{Engine, Raw};
use ivm_ir::{Frontier, Op, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::{Frontier as SqlFrontier, Sqlite};
use rusqlite::{Connection, OpenFlags};

fn mint_program(source: &str, output: &str) -> Program {
    Program {
        rels: vec![
            Relation { id: 0, name: source.into(), cols: vec![Ty::Int, Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "shared_pair".into(), cols: vec![Ty::Id, Ty::Int, Ty::Int], kind: RelKind::Constructor },
            Relation { id: 2, name: output.into(), cols: vec![Ty::Int, Ty::Int, Ty::Id], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::Mint { input: 0, functor: 1, args: vec![0, 1] }],
        strata: vec![Stratum::Let { id: 2, body: 1 }],
        outputs: vec![2],
    }
}

fn change(row: [i64; 2], w: i64) -> Frontier {
    Frontier { changes: vec![SourceChange { rel: 0, row: row.to_vec(), w }] }
}

#[test]
fn dictionary_survives_retraction_and_new_program_on_fresh_connection() {
    let uri = format!("file:mint-scope-{}?mode=memory&cache=shared", std::process::id());
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI;
    let db = Connection::open_with_flags(&uri, flags).unwrap();
    let mut first = Sqlite::install(&mint_program("first_source", "first_output"), &mut Raw::with_connection(&db)).unwrap();
    first.settle(change([1, 3], 1), &mut Raw::with_connection(&db)).unwrap();
    first.settle(change([1, 2], 1), &mut Raw::with_connection(&db)).unwrap();
    assert_eq!(first.intern_snapshot(1, &mut Raw::with_connection(&db)).unwrap(),
        vec![(vec![1, 1, 3], 1), (vec![2, 1, 2], 1)]);
    let lt: i64 = db.query_row("SELECT ivm_term_lt(2,1)", [], |row| row.get(0)).unwrap();
    assert_eq!(lt, 1);
    first.settle(change([1, 2], -1), &mut Raw::with_connection(&db)).unwrap();
    assert_eq!(first.intern_snapshot(1, &mut Raw::with_connection(&db)).unwrap().len(), 2);
    drop(first);

    let reopened = Connection::open_with_flags(&uri, flags).unwrap();
    let mut second = Sqlite::install(&mint_program("second_source", "second_output"), &mut Raw::with_connection(&reopened)).unwrap();
    let delta = second.settle(change([1, 2], 1), &mut Raw::with_connection(&reopened)).unwrap();
    assert_eq!(delta.changes, vec![(2, vec![1, 2, 2], 1)]);
    assert_eq!(second.intern_snapshot(1, &mut Raw::with_connection(&reopened)).unwrap().len(), 2);
}

#[test]
fn legacy_sql_catalog_reattaches_after_dictionary_schema_is_added() {
    let uri = format!("file:mint-legacy-{}?mode=memory&cache=shared", std::process::id());
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI;
    let db = Connection::open_with_flags(&uri, flags).unwrap();
    db.execute_batch("CREATE TABLE legacy_source(c0 INTEGER NOT NULL);").unwrap();
    let legacy = ivm_sqlite::Program::install(&db, "legacy_view", "SELECT c0 FROM legacy_source").unwrap();
    let mut minted = Sqlite::install(&mint_program("new_source", "new_output"), &mut Raw::with_connection(&db)).unwrap();
    minted.settle(change([4, 5], 1), &mut Raw::with_connection(&db)).unwrap();
    let reopened = Connection::open_with_flags(&uri, flags).unwrap();
    let attached = ivm_sqlite::Program::reattach(&reopened, "legacy_view").unwrap();
    assert_eq!(attached.frontier_id(&reopened).unwrap(), legacy.frontier_id(&db).unwrap());
    assert_eq!(minted.intern_snapshot(1, &mut Raw::with_connection(&db)).unwrap(), vec![(vec![1, 4, 5], 1)]);
}
