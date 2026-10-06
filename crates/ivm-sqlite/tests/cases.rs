//! Library gates: both packet cases end-to-end over the real settle path, the
//! rollback and failure semantics, and the explicit `Unsupported` surface.
//!
//! The oracle sequences run source writes through plain SQL so the installed
//! collector settles them at `COMMIT`, exactly like the extension path. The
//! direct-batch tests drive `Frontier::settle` with `SourceChange` values, the
//! linked-caller path.

use ivm_sqlite::{Cell, Frontier, Program, Sign, SourceChange};
use rusqlite::Connection;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
         CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
         CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
         CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);",
    )
    .unwrap();
    conn
}

const ACCESS_SQL: &str = "SELECT person, resource FROM direct_grant \
     UNION \
     SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team";
const TEAM_COST_SQL: &str =
    "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team";

fn install_access(conn: &Connection) -> Program {
    Program::install(conn, "access", ACCESS_SQL).unwrap()
}

fn install_team_cost(conn: &Connection) -> Program {
    Program::install(conn, "team_cost", TEAM_COST_SQL).unwrap()
}

#[test]
fn typed_program_reinstalls_from_catalog_ir() {
    let conn = conn();
    let sql_program = install_access(&conn);
    let json: String = conn
        .query_row(
            "SELECT program FROM frontier_catalog WHERE name='access'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let ir: ivm_ir::Program = serde_json::from_str(&json).unwrap();
    sql_program.teardown(&conn).unwrap();

    let typed = Program::install_ir(&conn, "access", &ir).unwrap();
    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1, 10);
         INSERT INTO permission VALUES (10, 100);
         COMMIT;",
    )
    .unwrap();
    let rows: Vec<(i64, i64)> = conn
        .prepare("SELECT c0, c1 FROM frontier_access")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, [(1, 100)]);
    assert_eq!(typed.frontier_id(&conn).unwrap(), 1);
    let reopened = Program::open(&conn, "access").unwrap();
    assert_eq!(
        reopened.snapshot(&conn).unwrap(),
        typed.snapshot(&conn).unwrap()
    );
}

#[test]
fn typed_map_filter_settles_and_reattaches() {
    use ivm_ir::{Expr, Func, Op, RelKind, Relation, Stratum, Ty};
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(a INTEGER NOT NULL, b INTEGER NOT NULL)")
        .unwrap();
    conn.execute_batch("INSERT INTO t VALUES (1,10)").unwrap();
    let program = ivm_ir::Program {
        terms: vec![],
        texts: vec![],
        rels: vec![
            Relation {
                id: 0,
                name: "t".into(),
                cols: vec![Ty::Int, Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "mapped".into(),
                cols: vec![Ty::Int, Ty::Int],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Mfp {
                input: 0,
                filter: vec![Expr::Call(Func::Eq, vec![Expr::Col(0), Expr::Lit(1)])],
                map: vec![Expr::Call(Func::Add, vec![Expr::Col(1), Expr::Lit(5)])],
                project: vec![0, 2],
            },
            Op::Union(vec![1]),
        ],
        strata: vec![Stratum::Let { id: 1, body: 2 }],
        outputs: vec![1],
    };
    let installed = Program::install_ir(&conn, "mapped", &program).unwrap();
    let snapshot = |p: &Program| p.snapshot(&conn).unwrap();
    assert_eq!(
        snapshot(&installed),
        vec![ivm_sqlite::Tuple(vec![Cell::Integer(1), Cell::Integer(15)])]
    );
    conn.execute_batch("BEGIN; INSERT INTO t VALUES (2,20); COMMIT;")
        .unwrap();
    assert_eq!(
        snapshot(&installed),
        vec![ivm_sqlite::Tuple(vec![Cell::Integer(1), Cell::Integer(15)])]
    );
    conn.execute_batch("BEGIN; DELETE FROM t WHERE a=1; INSERT INTO t VALUES (1,11); COMMIT;")
        .unwrap();
    let reopened = Program::open(&conn, "mapped").unwrap();
    assert_eq!(
        snapshot(&reopened),
        vec![ivm_sqlite::Tuple(vec![Cell::Integer(1), Cell::Integer(16)])]
    );
}

#[test]
fn typed_map_filter_over_join_uses_join_delta() {
    use ivm_ir::{Expr, Func, Op, RelKind, Relation, Stratum, Ty};
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE l(a INTEGER NOT NULL, k INTEGER NOT NULL); CREATE TABLE r(k INTEGER NOT NULL, b INTEGER NOT NULL);").unwrap();
    let program = ivm_ir::Program {
        terms: vec![],
        texts: vec![],
        rels: vec![
            Relation {
                id: 0,
                name: "l".into(),
                cols: vec![Ty::Int, Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "r".into(),
                cols: vec![Ty::Int, Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 2,
                name: "mapped".into(),
                cols: vec![Ty::Int, Ty::Int],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![
            Op::Get(0),
            Op::Get(1),
            Op::Join {
                inputs: vec![0, 1],
                equivalences: vec![vec![(0, 1), (1, 0)]],
                project: vec![],
            },
            Op::Mfp {
                input: 2,
                filter: vec![Expr::Call(Func::Eq, vec![Expr::Col(0), Expr::Lit(1)])],
                map: vec![Expr::Call(Func::Add, vec![Expr::Col(3), Expr::Lit(2)])],
                project: vec![0, 4],
            },
            Op::Union(vec![3]),
        ],
        strata: vec![Stratum::Let { id: 2, body: 4 }],
        outputs: vec![2],
    };
    let installed = Program::install_ir(&conn, "mapped", &program).unwrap();
    conn.execute_batch(
        "BEGIN; INSERT INTO l VALUES (1,10),(2,10); INSERT INTO r VALUES (10,100); COMMIT;",
    )
    .unwrap();
    assert_eq!(
        installed.snapshot(&conn).unwrap(),
        vec![ivm_sqlite::Tuple(vec![
            Cell::Integer(1),
            Cell::Integer(102)
        ])]
    );
    conn.execute_batch("BEGIN; DELETE FROM r WHERE b=100; INSERT INTO r VALUES (10,101); COMMIT;")
        .unwrap();
    assert_eq!(
        Program::open(&conn, "mapped")
            .unwrap()
            .snapshot(&conn)
            .unwrap(),
        vec![ivm_sqlite::Tuple(vec![
            Cell::Integer(1),
            Cell::Integer(103)
        ])]
    );
}

#[test]
fn typed_threshold_tracks_duplicate_support() {
    let conn = conn();
    let ir: ivm_ir::Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/0_access.program.json")).unwrap();
    let installed = Program::install_ir(&conn, "access", &ir).unwrap();
    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1,10);
         INSERT INTO permission VALUES (10,100);
         INSERT INTO direct_grant VALUES (1,100);
         COMMIT;",
    )
    .unwrap();
    let want = vec![ivm_sqlite::Tuple(vec![
        Cell::Integer(1),
        Cell::Integer(100),
    ])];
    assert_eq!(installed.snapshot(&conn).unwrap(), want);
    conn.execute_batch("BEGIN; DELETE FROM direct_grant; COMMIT;")
        .unwrap();
    assert_eq!(
        Program::open(&conn, "access")
            .unwrap()
            .snapshot(&conn)
            .unwrap(),
        want
    );
    conn.execute_batch("BEGIN; DELETE FROM permission; COMMIT;")
        .unwrap();
    assert!(installed.snapshot(&conn).unwrap().is_empty());
}

#[test]
fn typed_threshold_over_source_preserves_one_visible_row() {
    use ivm_ir::{Op, RelKind, Relation, Stratum, Ty};
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(a INTEGER NOT NULL); INSERT INTO t VALUES (7),(7);")
        .unwrap();
    let ir = ivm_ir::Program {
        terms: vec![],
        texts: vec![],
        rels: vec![
            Relation {
                id: 0,
                name: "t".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Source,
            },
            Relation {
                id: 1,
                name: "distinct_t".into(),
                cols: vec![Ty::Int],
                kind: RelKind::Derived,
            },
        ],
        nodes: vec![Op::Get(0), Op::Threshold(0)],
        strata: vec![Stratum::Let { id: 1, body: 1 }],
        outputs: vec![1],
    };
    let installed = Program::install_ir(&conn, "distinct_t", &ir).unwrap();
    let want = vec![ivm_sqlite::Tuple(vec![Cell::Integer(7)])];
    assert_eq!(installed.snapshot(&conn).unwrap(), want);
    conn.execute_batch("BEGIN; DELETE FROM t WHERE rowid=(SELECT min(rowid) FROM t); COMMIT;")
        .unwrap();
    assert_eq!(installed.snapshot(&conn).unwrap(), want);
    conn.execute_batch("BEGIN; DELETE FROM t; COMMIT;").unwrap();
    assert!(installed.snapshot(&conn).unwrap().is_empty());
}

#[test]
fn typed_antijoin_tracks_right_key_and_left_filter() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL); \
        CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL); \
        INSERT INTO membership VALUES (1,11),(2,9),(3,12);",
    )
    .unwrap();
    let ir: ivm_ir::Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/4_antijoin.program.json")).unwrap();
    let installed = Program::install_ir(&conn, "lonely", &ir).unwrap();
    let rows = |p: &Program| p.snapshot(&conn).unwrap();
    let tuple = |person, team| ivm_sqlite::Tuple(vec![Cell::Integer(person), Cell::Integer(team)]);
    assert_eq!(rows(&installed), vec![tuple(1, 11), tuple(3, 12)]);
    conn.execute_batch("BEGIN; INSERT INTO direct_grant VALUES (1,100); COMMIT;")
        .unwrap();
    assert_eq!(rows(&installed), vec![tuple(3, 12)]);
    conn.execute_batch("BEGIN; INSERT INTO membership VALUES (1,13); DELETE FROM membership WHERE person=3; COMMIT;").unwrap();
    assert!(rows(&installed).is_empty());
    conn.execute_batch("BEGIN; DELETE FROM direct_grant WHERE person=1; COMMIT;")
        .unwrap();
    let reopened = Program::open(&conn, "lonely").unwrap();
    assert_eq!(rows(&reopened), vec![tuple(1, 11), tuple(1, 13)]);
    conn.execute_batch("BEGIN; INSERT INTO direct_grant VALUES (1,100),(1,101); COMMIT;")
        .unwrap();
    assert!(rows(&reopened).is_empty());
    conn.execute_batch("BEGIN; DELETE FROM direct_grant WHERE resource=100; COMMIT;")
        .unwrap();
    assert!(rows(&reopened).is_empty());
    conn.execute_batch(
        "BEGIN; DELETE FROM direct_grant WHERE resource=101; \
        INSERT INTO membership VALUES (4,14); INSERT INTO direct_grant VALUES (4,200); COMMIT;",
    )
    .unwrap();
    assert_eq!(rows(&reopened), vec![tuple(1, 11), tuple(1, 13)]);
}

#[test]
fn typed_topk_reranks_touched_groups() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE job(id INTEGER NOT NULL, team INTEGER NOT NULL, cost INTEGER NOT NULL); \
        INSERT INTO job VALUES (1,10,100),(2,10,90),(3,20,50);",
    )
    .unwrap();
    let mut ir: ivm_ir::Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/6_topk.program.json")).unwrap();
    ir.strata.truncate(1);
    ir.outputs.truncate(1);
    let installed = Program::install_ir(&conn, "top_job", &ir).unwrap();
    let row = |id, team, cost| {
        ivm_sqlite::Tuple(vec![
            Cell::Integer(id),
            Cell::Integer(team),
            Cell::Integer(cost),
        ])
    };
    assert_eq!(
        installed.snapshot(&conn).unwrap(),
        vec![row(1, 10, 100), row(3, 20, 50)]
    );
    conn.execute_batch("BEGIN; INSERT INTO job VALUES (4,10,110),(5,20,40); COMMIT;")
        .unwrap();
    assert_eq!(
        installed.snapshot(&conn).unwrap(),
        vec![row(3, 20, 50), row(4, 10, 110)]
    );
    conn.execute_batch(
        "BEGIN; DELETE FROM job WHERE id=4; INSERT INTO job VALUES (6,10,100); COMMIT;",
    )
    .unwrap();
    let reopened = Program::open(&conn, "top_job").unwrap();
    assert_eq!(
        reopened.snapshot(&conn).unwrap(),
        vec![row(1, 10, 100), row(3, 20, 50)]
    );
    conn.execute_batch("BEGIN; DELETE FROM job WHERE id=1; COMMIT;")
        .unwrap();
    assert_eq!(
        reopened.snapshot(&conn).unwrap(),
        vec![row(3, 20, 50), row(6, 10, 100)]
    );
}

#[test]
fn typed_reduce_min_max_retracts_extrema() {
    let conn = conn();
    conn.execute_batch("INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,3);")
        .unwrap();
    let mut ir: ivm_ir::Program =
        serde_json::from_str(include_str!("../../ivm-dd/oracle/1_team_cost.program.json")).unwrap();
    ir.strata.truncate(1);
    ir.outputs.truncate(1);
    let installed = Program::install_ir(&conn, "team_cost_ir", &ir).unwrap();
    let tuple = |team, n, sum, min, max| {
        ivm_sqlite::Tuple(vec![
            Cell::Integer(team),
            Cell::Integer(n),
            Cell::Integer(sum),
            Cell::Integer(min),
            Cell::Integer(max),
        ])
    };
    assert_eq!(
        installed.snapshot(&conn).unwrap(),
        vec![tuple(10, 2, 12, 5, 7), tuple(20, 1, 3, 3, 3)]
    );
    conn.execute_batch(
        "BEGIN; DELETE FROM job WHERE id=1; INSERT INTO job VALUES (4,10,9); COMMIT;",
    )
    .unwrap();
    assert_eq!(
        installed.snapshot(&conn).unwrap(),
        vec![tuple(10, 2, 16, 7, 9), tuple(20, 1, 3, 3, 3)]
    );
    conn.execute_batch("BEGIN; DELETE FROM job WHERE team=20; COMMIT;")
        .unwrap();
    let reopened = Program::open(&conn, "team_cost_ir").unwrap();
    assert_eq!(
        reopened.snapshot(&conn).unwrap(),
        vec![tuple(10, 2, 16, 7, 9)]
    );
    conn.execute_batch("BEGIN; DELETE FROM job WHERE team=10; COMMIT;")
        .unwrap();
    assert!(reopened.snapshot(&conn).unwrap().is_empty());
}
fn visible_pairs(conn: &Connection) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT person, resource FROM frontier_access ORDER BY person, resource")
        .unwrap();
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}

fn visible_groups(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT team, jobs, total_cost FROM frontier_team_cost ORDER BY team")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn access_delta(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT __sign, person, resource FROM frontier_access_delta")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn team_cost_delta(conn: &Connection) -> Vec<(i64, i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT __sign, team, jobs, total_cost FROM frontier_team_cost_delta ORDER BY team, __sign")
        .unwrap();
    stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn engine_snapshot_pairs(program: &Program, conn: &Connection) -> Vec<(i64, i64)> {
    let mut rows: Vec<(i64, i64)> = program
        .snapshot(conn)
        .unwrap()
        .into_iter()
        .map(|t| {
            (
                match t.0[0] {
                    Cell::Integer(v) => v,
                    ref other => panic!("non-integer cell {other:?}"),
                },
                match t.0[1] {
                    Cell::Integer(v) => v,
                    ref other => panic!("non-integer cell {other:?}"),
                },
            )
        })
        .collect();
    rows.sort_unstable();
    rows
}

#[track_caller]
fn assert_pairs(got: &[(i64, i64)], want: &[(i64, i64)]) {
    let mut g = got.to_vec();
    g.sort_unstable();
    assert_eq!(g, want, "visible pairs mismatch");
}

#[track_caller]
fn assert_delta_pairs(got: &[(i64, i64, i64)], want: &[(i64, i64, i64)]) {
    let mut g = got.to_vec();
    g.sort_unstable();
    assert_eq!(g, want, "delta mismatch");
}

#[test]
fn case1_full_oracle_sequence() {
    let conn = conn();
    let program = install_access(&conn);

    // 0_initial
    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1,10),(1,20);
         INSERT INTO permission VALUES (10,100),(20,100);
         INSERT INTO direct_grant VALUES (3,300);
         COMMIT;",
    )
    .unwrap();
    assert_pairs(&visible_pairs(&conn), &[(1, 100), (3, 300)]);
    assert_delta_pairs(&access_delta(&conn), &[(1, 1, 100), (1, 3, 300)]);
    assert_eq!(program.frontier_id(&conn).unwrap(), 1);
    assert_eq!(
        engine_snapshot_pairs(&program, &conn),
        vec![(1, 100), (3, 300)]
    );

    // 1_both_join_inputs — the cross-term frontier
    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (2,10);
         INSERT INTO permission VALUES (10,200);
         COMMIT;",
    )
    .unwrap();
    assert_pairs(
        &visible_pairs(&conn),
        &[(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)],
    );
    assert_delta_pairs(
        &access_delta(&conn),
        &[(1, 1, 200), (1, 2, 100), (1, 2, 200)],
    );
    assert_eq!(program.frontier_id(&conn).unwrap(), 2);

    // 2_duplicate_union_support: silent 2 -> 1 support
    conn.execute_batch("INSERT INTO direct_grant VALUES (1,200);")
        .unwrap();
    assert_pairs(
        &visible_pairs(&conn),
        &[(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)],
    );
    assert_delta_pairs(&access_delta(&conn), &[]);
    assert_eq!(program.frontier_id(&conn).unwrap(), 3);

    // 3_join_support_retract: (1,100) support 2 -> 1, still visible
    conn.execute_batch("DELETE FROM membership WHERE person=1 AND team=10;")
        .unwrap();
    assert_pairs(
        &visible_pairs(&conn),
        &[(1, 100), (1, 200), (2, 100), (2, 200), (3, 300)],
    );
    assert_delta_pairs(&access_delta(&conn), &[]);

    // 4_last_join_support: (1,100) 1 -> 0, retractions only
    conn.execute_batch("DELETE FROM permission WHERE team=20 AND resource=100;")
        .unwrap();
    assert_pairs(
        &visible_pairs(&conn),
        &[(1, 200), (2, 100), (2, 200), (3, 300)],
    );
    assert_delta_pairs(&access_delta(&conn), &[(-1, 1, 100)]);

    // 5_last_union_support
    conn.execute_batch("DELETE FROM direct_grant WHERE person=1 AND resource=200;")
        .unwrap();
    assert_pairs(&visible_pairs(&conn), &[(2, 100), (2, 200), (3, 300)]);
    assert_delta_pairs(&access_delta(&conn), &[(-1, 1, 200)]);

    // 6_savepoint_rollback: nothing settles, frontier id does not move
    let before = program.frontier_id(&conn).unwrap();
    conn.execute_batch(
        "BEGIN;
         SAVEPOINT discarded;
         INSERT INTO direct_grant VALUES (4,400);
         ROLLBACK TO discarded;
         RELEASE discarded;
         COMMIT;",
    )
    .unwrap();
    assert_pairs(&visible_pairs(&conn), &[(2, 100), (2, 200), (3, 300)]);
    assert_eq!(program.frontier_id(&conn).unwrap(), before);

    // 7_transaction_rollback
    conn.execute_batch("BEGIN; INSERT INTO membership VALUES (5,10); ROLLBACK;")
        .unwrap();
    assert_pairs(&visible_pairs(&conn), &[(2, 100), (2, 200), (3, 300)]);
    assert_eq!(program.frontier_id(&conn).unwrap(), before);

    // 8_update
    conn.execute_batch("UPDATE permission SET resource=300 WHERE team=10 AND resource=200;")
        .unwrap();
    assert_pairs(&visible_pairs(&conn), &[(2, 100), (2, 300), (3, 300)]);
    assert_delta_pairs(&access_delta(&conn), &[(-1, 2, 200), (1, 2, 300)]);
}

#[test]
fn case2_full_oracle_sequence() {
    let conn = conn();
    let program = install_team_cost(&conn);

    // 0_initial
    conn.execute_batch("BEGIN; INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11); COMMIT;")
        .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 2, 12), (20, 1, 11)]);
    assert_eq!(team_cost_delta(&conn), vec![(1, 10, 2, 12), (1, 20, 1, 11)]);

    // 1_move_and_add: one frontier moves job 3 and adds job 4
    conn.execute_batch(
        "BEGIN;
         INSERT INTO job VALUES (4,10,3);
         UPDATE job SET team=10 WHERE id=3;
         COMMIT;",
    )
    .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 4, 26)]);
    assert_eq!(
        team_cost_delta(&conn),
        vec![(-1, 10, 2, 12), (1, 10, 4, 26), (-1, 20, 1, 11)]
    );

    // 2_cross_zero: cost 7 -> -7
    conn.execute_batch("UPDATE job SET cost=-7 WHERE id=2;")
        .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 4, 12)]);
    assert_eq!(
        team_cost_delta(&conn),
        vec![(-1, 10, 4, 26), (1, 10, 4, 12)]
    );

    // 3_delete_two
    conn.execute_batch("BEGIN; DELETE FROM job WHERE id IN (1,4); COMMIT;")
        .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 2, 4)]);
    assert_eq!(team_cost_delta(&conn), vec![(-1, 10, 4, 12), (1, 10, 2, 4)]);

    // 4_empty: last job of the group leaves; delta still records it
    conn.execute_batch("DELETE FROM job WHERE id IN (2,3);")
        .unwrap();
    assert!(visible_groups(&conn).is_empty());
    assert_eq!(team_cost_delta(&conn), vec![(-1, 10, 2, 4)]);
    assert_eq!(program.frontier_id(&conn).unwrap(), 5);

    // 5_rollback: no new committed state
    let before = program.frontier_id(&conn).unwrap();
    conn.execute_batch("BEGIN; INSERT INTO job VALUES (5,30,9); ROLLBACK;")
        .unwrap();
    assert!(visible_groups(&conn).is_empty());
    assert_eq!(program.frontier_id(&conn).unwrap(), before);
}

#[test]
fn direct_settle_batch_contracts() {
    let conn = conn();
    let program = install_access(&conn);
    let ins = |t: &str, row: Vec<i64>| SourceChange {
        relation: t.to_string(),
        sign: Sign::Insert,
        row: row.into_iter().map(Cell::Integer).collect(),
    };
    let del = |t: &str, row: Vec<i64>| SourceChange {
        relation: t.to_string(),
        sign: Sign::Delete,
        row: row.into_iter().map(Cell::Integer).collect(),
    };
    // The direct-batch contract: the batch mirrors source changes that have
    // already landed in the tables, so apply them before settling.
    fn apply(conn: &Connection, sql: &str) {
        conn.execute_batch(sql).unwrap();
    }

    // Net-zero batch: insert and delete of the same row in one frontier.
    let changes = vec![
        ins("membership", vec![2, 10]),
        del("membership", vec![2, 10]),
    ];
    let delta = program.settle(&conn, &changes).unwrap();
    assert!(delta.is_empty());
    assert_pairs(&visible_pairs(&conn), &[]);

    // Duplicates: a source table without a primary key, watched by its own
    // program, so one logical row can carry multiple supports.
    conn.execute_batch(
        "CREATE TABLE log_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL);",
    )
    .unwrap();
    let log_program = Program::install(
        &conn,
        "log_access",
        "SELECT person, resource FROM log_grant",
    )
    .unwrap();
    let log_pairs = |conn: &Connection| -> Vec<(i64, i64)> {
        let mut stmt = conn
            .prepare("SELECT person, resource FROM frontier_log_access ORDER BY person, resource")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let log_delta = |conn: &Connection| -> Vec<(i64, i64, i64)> {
        let mut stmt = conn
            .prepare("SELECT __sign, person, resource FROM frontier_log_access_delta")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    // Two identical rows land through SQL: the collector settles them, the
    // view shows the pair once, and the frontier's delta is a single insert.
    apply(&conn, "INSERT INTO log_grant VALUES (1,100),(1,100);");
    assert_eq!(log_delta(&conn), vec![(1, 1, 100)]);
    assert_eq!(log_pairs(&conn), vec![(1, 100)]);

    // One duplicate leaves: 2 -> 1, silent.
    apply(
        &conn,
        "DELETE FROM log_grant WHERE rowid = (SELECT min(rowid) FROM log_grant WHERE person=1 AND resource=100);",
    );
    assert!(log_delta(&conn).is_empty());
    assert_eq!(log_pairs(&conn), vec![(1, 100)]);

    // Last duplicate leaves: 1 -> 0, retraction.
    apply(
        &conn,
        "DELETE FROM log_grant WHERE person=1 AND resource=100;",
    );
    assert_eq!(log_delta(&conn), vec![(-1, 1, 100)]);
    assert!(log_pairs(&conn).is_empty());
    assert_eq!(log_program.frontier_id(&conn).unwrap(), 3);

    // Empty batch is a valid frontier.
    let frontier = program.frontier_id(&conn).unwrap();
    let delta = program.settle(&conn, &[]).unwrap();
    assert!(delta.is_empty());
    assert_eq!(program.frontier_id(&conn).unwrap(), frontier + 1);

    // Unknown relation.
    let err = program
        .settle(&conn, &[ins("job", vec![1, 10, 5])])
        .unwrap_err();
    assert!(matches!(
        err.kind,
        ivm_sqlite::ErrorKind::UnknownRelation(_)
    ));

    // Arity mismatch.
    let err = program
        .settle(&conn, &[ins("membership", vec![1])])
        .unwrap_err();
    assert!(matches!(
        err.kind,
        ivm_sqlite::ErrorKind::Arity {
            expected: 2,
            got: 1,
            ..
        }
    ));

    // The source schema declares this column NOT NULL, so SQLite rejects it.
    let err = program
        .settle(
            &conn,
            &[SourceChange {
                relation: "membership".into(),
                sign: Sign::Insert,
                row: vec![Cell::Integer(1), Cell::Null],
            }],
        )
        .unwrap_err();
    assert!(matches!(err.kind, ivm_sqlite::ErrorKind::Sqlite(_)));
}

#[test]
fn callback_failure_fails_commit_keeps_previous_state() {
    let conn = conn();
    let program = install_team_cost(&conn);
    conn.execute_batch("INSERT INTO job VALUES (1,10,5);")
        .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 1, 5)]);
    let frontier = program.frontier_id(&conn).unwrap();

    // Two distinct near-max costs in one frontier: the weighted sum's inputs
    // stay integers, so sum() itself raises SQLite's integer overflow, the
    // collector callback fails, and the COMMIT must fail. (Two identical max
    // costs would instead net to one staged row whose 2*cost product widens
    // to REAL silently — SQLite's own expression semantics.)
    let result = conn.execute_batch(
        "BEGIN;
         INSERT INTO job VALUES (2,10,9223372036854775807);
         INSERT INTO job VALUES (3,10,9223372036854775806);
         COMMIT;",
    );
    assert!(result.is_err(), "commit with overflowing settle must fail");
    let _ = conn.execute_batch("ROLLBACK;");

    // Previous committed state is readable and the connection still works.
    assert_eq!(visible_groups(&conn), vec![(10, 1, 5)]);
    assert_eq!(program.frontier_id(&conn).unwrap(), frontier);
    conn.execute_batch("INSERT INTO job VALUES (4,20,7);")
        .unwrap();
    assert_eq!(visible_groups(&conn), vec![(10, 1, 5), (20, 1, 7)]);
}

#[test]
fn unsupported_program_shapes_are_explicit() {
    let conn = conn();
    let shapes = [
        "SELECT person FROM direct_grant WHERE person > 2",
        "SELECT person, avg(resource) FROM direct_grant GROUP BY person",
        "SELECT person, min(resource) FROM direct_grant GROUP BY person",
        "SELECT person, resource FROM direct_grant ORDER BY resource",
        "SELECT person, resource FROM direct_grant LIMIT 2",
        "SELECT person, resource FROM direct_grant, permission",
        "SELECT d.person, p.resource FROM direct_grant d CROSS JOIN permission p",
        "SELECT team, count(cost) FROM job GROUP BY team",
        "SELECT m.person, count(*) FROM membership m JOIN permission p ON p.team = m.team GROUP BY m.person",
        "SELECT person, resource FROM direct_grant UNION SELECT person, resource FROM direct_grant WHERE person IS NOT NULL",
    ];
    for (i, sql) in shapes.iter().enumerate() {
        let err = Program::install(&conn, &format!("bad{i}"), sql).unwrap_err();
        assert!(
            err.is_unsupported(),
            "shape {i} should be Unsupported, got {err}"
        );
    }

    // Duplicate install is a state conflict, not an unsupported shape.
    let _program = install_access(&conn);
    let err = Program::install(&conn, "access", ACCESS_SQL).unwrap_err();
    assert!(!err.is_unsupported());
    assert!(matches!(err.kind, ivm_sqlite::ErrorKind::State(_)));

    // Unknown program on open.
    let err = Program::open(&conn, "missing").unwrap_err();
    assert!(matches!(err.kind, ivm_sqlite::ErrorKind::State(_)));
}

#[test]
fn open_reload_and_teardown_lifecycle() {
    let conn = conn();
    let program = install_access(&conn);
    conn.execute_batch("INSERT INTO direct_grant VALUES (3,300);")
        .unwrap();

    // A second handle, reopened from the catalog, drives the same program.
    let reopened = Program::open(&conn, "access").unwrap();
    assert_eq!(reopened.name(), "access");
    assert_eq!(reopened.output_columns().len(), 2);
    assert_eq!(
        program.frontier_id(&conn).unwrap(),
        reopened.frontier_id(&conn).unwrap()
    );
    let _ = reopened.settle(&conn, &[]).unwrap();
    assert_eq!(program.frontier_id(&conn).unwrap(), 2);

    // Teardown removes every engine object and leaves the sources alone.
    program.teardown(&conn).unwrap();
    let leftovers: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'frontier\\_access%' ESCAPE '\\'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0, "engine objects must be gone");
    let members: i64 = conn
        .query_row("SELECT count(*) FROM direct_grant", [], |row| row.get(0))
        .unwrap();
    assert_eq!(members, 1, "source rows stay");

    // After teardown the name is free again.
    install_access(&conn).frontier_id(&conn).unwrap();
}
