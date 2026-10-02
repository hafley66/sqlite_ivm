use ivm_engine::Engine;
use ivm_ir::{Frontier, Op, Order, Program, RelKind, Relation, SourceChange, Stratum, Ty};
use ivm_sqlite::{Frontier as SqlFrontier, Sqlite};
use ivm_dd::Dd;
use rusqlite::{Connection, OpenFlags};

fn mint_program(source: &str, output: &str) -> Program {
    Program {
        texts: vec![],
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
    let mut first = Sqlite::install_on(Connection::open_with_flags(&uri, flags).unwrap(), &mint_program("first_source", "first_output")).unwrap();
    first.settle(change([1, 3], 1)).unwrap();
    first.settle(change([1, 2], 1)).unwrap();
    assert_eq!(first.intern_snapshot(1).unwrap(),
        vec![(vec![1, 1, 3], 1), (vec![2, 1, 2], 1)]);
    let lt: i64 = first.db.query_row("SELECT ivm_term_lt(2,1)", [], |row| row.get(0)).unwrap();
    assert_eq!(lt, 1);
    first.settle(change([1, 2], -1)).unwrap();
    assert_eq!(first.intern_snapshot(1).unwrap().len(), 2);
    drop(first);

    let reopened = Connection::open_with_flags(&uri, flags).unwrap();
    let _attached = ivm_sqlite::Program::open(&reopened, "first_output").unwrap();
    let reopened_lt: i64 = reopened.query_row("SELECT ivm_term_lt(2,1)", [], |row| row.get(0)).unwrap();
    assert_eq!(reopened_lt, 1);
    let mut second = Sqlite::install_on(reopened, &mint_program("second_source", "second_output")).unwrap();
    let delta = second.settle(change([1, 2], 1)).unwrap();
    assert_eq!(delta.changes, vec![(2, vec![1, 2, 2], 1)]);
    assert_eq!(second.intern_snapshot(1).unwrap().len(), 2);
}

#[test]
fn legacy_sql_catalog_reattaches_after_dictionary_schema_is_added() {
    let uri = format!("file:mint-legacy-{}?mode=memory&cache=shared", std::process::id());
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI;
    let db = Connection::open_with_flags(&uri, flags).unwrap();
    db.execute_batch("CREATE TABLE legacy_source(c0 INTEGER NOT NULL);").unwrap();
    let legacy = ivm_sqlite::Program::install(&db, "legacy_view", "SELECT c0 FROM legacy_source").unwrap();
    let mut minted = Sqlite::install_on(Connection::open_with_flags(&uri, flags).unwrap(), &mint_program("new_source", "new_output")).unwrap();
    minted.settle(change([4, 5], 1)).unwrap();
    let reopened = Connection::open_with_flags(&uri, flags).unwrap();
    let attached = ivm_sqlite::Program::reattach(&reopened, "legacy_view").unwrap();
    assert_eq!(attached.frontier_id(&reopened).unwrap(), legacy.frontier_id(&db).unwrap());
    assert_eq!(minted.intern_snapshot(1).unwrap(), vec![(vec![1, 4, 5], 1)]);
}

#[test]
fn failed_constructor_write_rolls_back_source_and_dictionary() {
    let mut engine = Sqlite::install(&mint_program("rollback_source", "rollback_output")).unwrap();
    let ctor: String = engine.db.query_row("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'ivm_ctor_%'", [], |row| row.get(0)).unwrap();
    engine.db.execute_batch(&format!("CREATE TRIGGER fail_ctor BEFORE INSERT ON \"{ctor}\" BEGIN SELECT RAISE(FAIL, 'injected'); END;")).unwrap();
    assert!(engine.settle(change([7, 8], 1)).is_err());
    let dict_rows: i64 = engine.db.query_row("SELECT count(*) FROM ivm_term", [], |row| row.get(0)).unwrap();
    let source_rows: i64 = engine.db.query_row("SELECT count(*) FROM rollback_source", [], |row| row.get(0)).unwrap();
    assert_eq!((dict_rows, source_rows), (0, 0));
    engine.db.execute_batch("DROP TRIGGER fail_ctor").unwrap();
    let delta = engine.settle(change([7, 8], 1)).unwrap();
    assert_eq!(delta.changes, vec![(2, vec![7, 8, 1], 1)]);
}

#[test]
fn constructor_and_mint_paths_consolidate_one_delta() {
    let program = Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "twice_source".into(), cols: vec![Ty::Int], kind: RelKind::Source },
            Relation { id: 1, name: "twice_ctor".into(), cols: vec![Ty::Id, Ty::Int], kind: RelKind::Constructor },
            Relation { id: 2, name: "twice_output".into(), cols: vec![Ty::Id], kind: RelKind::Derived },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mint { input: 0, functor: 1, args: vec![0] },
            Op::Mfp { input: 1, filter: vec![], map: vec![], project: vec![1] },
            Op::Get(1),
            Op::Mfp { input: 3, filter: vec![], map: vec![], project: vec![0] },
            Op::Union(vec![2, 4]),
        ],
        strata: vec![Stratum::Let { id: 2, body: 5 }],
        outputs: vec![2],
    };
    let frontier = Frontier { changes: vec![SourceChange { rel: 0, row: vec![9], w: 1 }] };
    let mut dd = Dd::install(&program).unwrap();
    let mut sql = Sqlite::install(&program).unwrap();
    let a = dd.settle(frontier.clone()).unwrap();
    let b = sql.settle(frontier).unwrap();
    assert_eq!(a.changes, vec![(2, vec![1], 2)]);
    assert_eq!(b.changes, a.changes);
}

#[test]
fn topk_over_ids_uses_dictionary_order_across_programs() {
    let mut minted = Sqlite::install(&mint_program("order_terms", "order_terms_out")).unwrap();
    minted.settle(change([1, 3], 1)).unwrap();
    minted.settle(change([1, 2], 1)).unwrap();
    let program = Program {
        texts: vec![],
        rels: vec![
            Relation { id: 0, name: "order_ids".into(), cols: vec![Ty::Id], kind: RelKind::Source },
            Relation { id: 1, name: "order_min".into(), cols: vec![Ty::Id], kind: RelKind::Derived },
        ],
        nodes: vec![Op::Get(0), Op::TopK { input: 0, key: vec![], order: vec![Order { col: 0, desc: false }], limit: 1 }],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    };
    // Both programs share one dictionary: the second installs on the first one's database.
    let mut ordered = Sqlite::install_on(minted.db, &program).unwrap();
    for id in [1, 2] {
        ordered.settle(Frontier { changes: vec![SourceChange { rel: 0, row: vec![id], w: 1 }] }).unwrap();
    }
    assert_eq!(ordered.snapshot(1).unwrap(), vec![(vec![2], 1)]);
}
