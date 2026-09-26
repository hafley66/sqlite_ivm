//! Gate: installing over pre-populated sources materializes the current rows,
//! so the installed snapshot equals a fresh evaluation of the defining SELECT
//! and every later committed transaction still settles exactly.
//!
//! Bootstrap is set-wise: each source is read once by an `INSERT .. SELECT`,
//! the staged rows are netted through the engine's own fill statements, no
//! frontier occurs (the counter stays 0, the delta table stays empty), and a
//! failed install rolls back to a schema with no program objects and sources
//! byte-identical to before. Cells keep their storage classes end to end.

use ivm_sqlite::{Cell, Composition, EngineError, Frontier, Program, Tuple};
use rusqlite::types::Value;
use rusqlite::Connection;

/// The access program's sources, as in `tests/cases.rs`.
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
const JOIN_SQL: &str =
    "SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team";
const TEAM_COST_SQL: &str =
    "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team";

#[test]
fn old_sql_catalog_reattaches_as_persisted_ir() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ivm-sqlite-old-catalog-{}-{}.sqlite",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
             CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
             CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));",
        ).unwrap();
        let installed = Program::install(&conn, "access", ACCESS_SQL).unwrap();
        conn.execute_batch("INSERT INTO direct_grant VALUES (1,100);")
            .unwrap();
        assert_eq!(installed.frontier_id(&conn).unwrap(), 1);
        conn.execute_batch("ALTER TABLE frontier_catalog RENAME COLUMN program TO sql;")
            .unwrap();
        conn.execute(
            "UPDATE frontier_catalog SET sql = ?1 WHERE name = 'access'",
            [ACCESS_SQL],
        )
        .unwrap();
    }
    {
        let conn = Connection::open(&path).unwrap();
        let attached = Program::reattach(&conn, "access").unwrap();
        assert_eq!(attached.frontier_id(&conn).unwrap(), 1);
        assert_snapshot(
            &conn,
            &attached,
            "SELECT person, resource FROM direct_grant ORDER BY person, resource",
        );
        let json: String = conn
            .query_row(
                "SELECT program FROM frontier_catalog WHERE name='access'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let ir: ivm_ir::Program = serde_json::from_str(&json).unwrap();
        assert_eq!(ir.outputs.len(), 1);
        conn.execute_batch("INSERT INTO direct_grant VALUES (2,200);")
            .unwrap();
        assert_eq!(attached.frontier_id(&conn).unwrap(), 2);
        assert_snapshot(
            &conn,
            &attached,
            "SELECT person, resource FROM direct_grant ORDER BY person, resource",
        );
        attached.teardown(&conn).unwrap();
    }
    std::fs::remove_file(path).unwrap();
}

/// A fresh evaluation of `sql` over the live tables, as typed cells: the
/// oracle every snapshot is compared against. Both sides use SQLite's own
/// `ORDER BY`, so mixed storage classes order identically.
fn fresh(conn: &Connection, sql: &str) -> Vec<Vec<Cell>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let n = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            (0..n)
                .map(|i| row.get::<_, Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows.into_iter()
        .map(|row| row.into_iter().map(|v| cell(v)).collect())
        .collect()
}

fn cell(value: Value) -> Cell {
    match value {
        Value::Null => Cell::Null,
        Value::Integer(v) => Cell::Integer(v),
        Value::Real(v) => Cell::Real(v),
        Value::Text(v) => Cell::Text(v),
        Value::Blob(v) => Cell::Blob(v),
    }
}

fn snapshot(conn: &Connection, program: &Program) -> Vec<Vec<Cell>> {
    let rows: Vec<Tuple> = program.snapshot(conn).unwrap();
    rows.into_iter().map(|t| t.0).collect()
}

#[track_caller]
fn assert_snapshot(conn: &Connection, program: &Program, sql: &str) {
    let got = snapshot(conn, program);
    let want = fresh(conn, sql);
    assert_eq!(got, want, "snapshot diverged from fresh evaluation");
}

/// The frontier table's signed rows, in the engine's read order.
fn delta(conn: &Connection, name: &str, cols: usize) -> Vec<(i64, Vec<Cell>)> {
    let names: Vec<&str> = match (name, cols) {
        ("access", _) => vec!["person", "resource"],
        ("team_cost", _) => vec!["team", "jobs", "total_cost"],
        ("pairs", _) => vec!["person", "resource"],
        ("team_total", _) => vec!["team", "jobs", "total_cost"],
        _ => unreachable!(),
    };
    let select = names
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let sql =
        format!("SELECT __sign, {select} FROM frontier_{name}_delta ORDER BY {select}, __sign");
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            (1..=cols)
                .map(|i| cell(row.get(i).unwrap()))
                .collect::<Vec<_>>(),
        ))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn frontier_of(conn: &Connection, name: &str) -> u64 {
    conn.query_row(
        "SELECT frontier FROM frontier_catalog WHERE name = ?1",
        [name],
        |row| row.get::<_, i64>(0).map(|v| v as u64),
    )
    .unwrap()
}

/// Union-support weights straight from the root.
fn weights(conn: &Connection, name: &str) -> Vec<(Vec<Cell>, i64)> {
    let sql = format!(
        "SELECT person, resource, __weight FROM frontier_{name}_root ORDER BY person, resource"
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map([], |row| {
        Ok((
            vec![cell(row.get(0).unwrap()), cell(row.get(1).unwrap())],
            row.get::<_, i64>(2).unwrap(),
        ))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn group_state(conn: &Connection, name: &str) -> Vec<(i64, i64, i64)> {
    let sql = format!("SELECT team, __n, __s0 FROM frontier_{name}_root ORDER BY team");
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Every program-shaped object is gone; only the shared catalog tables may
/// remain (install creates them before the savepoint opens).
#[track_caller]
fn assert_no_program_objects(conn: &Connection) {
    let left: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'frontier\\_%' ESCAPE '\\' \
             AND name NOT IN ('frontier_catalog', 'frontier_catalog_column')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "program objects leaked");
}

fn catalog_rows(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM frontier_catalog WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[track_caller]
fn assert_install_rejected(result: Result<Program, EngineError>, unsupported: bool, needle: &str) {
    let err = result.err().expect("install must be rejected");
    let text = format!("{err}");
    assert_eq!(err.is_unsupported(), unsupported, "wrong kind: {text}");
    assert!(text.contains(needle), "message '{text}' lacks '{needle}'");
}

/// Pre-populated rows back a union program: duplicate supports seed the true
/// weight (2), so the first retract is silent (2->1) and the second emits the
/// net change (1->0). Fresh-SQL equality at every step.
#[test]
fn union_duplicate_supports_from_prepopulated_rows() {
    let conn = conn();
    conn.execute_batch(
        "INSERT INTO direct_grant VALUES (1, 100);
         INSERT INTO membership VALUES (1, 10);
         INSERT INTO permission VALUES (10, 100);",
    )
    .unwrap();
    let program = Program::install(&conn, "access", ACCESS_SQL).unwrap();

    assert_eq!(
        weights(&conn, "access"),
        vec![(vec![Cell::Integer(1), Cell::Integer(100)], 2)]
    );
    assert_snapshot(&conn, &program, "SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(frontier_of(&conn, "access"), 0);
    assert!(
        delta(&conn, "access", 2).is_empty(),
        "bootstrap emitted a delta"
    );

    // 2 -> 1: still visible, no delta, one frontier.
    conn.execute("DELETE FROM direct_grant WHERE person = 1", [])
        .unwrap();
    assert_snapshot(&conn, &program, "SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(
        weights(&conn, "access"),
        vec![(vec![Cell::Integer(1), Cell::Integer(100)], 1)]
    );
    assert!(
        delta(&conn, "access", 2).is_empty(),
        "2->1 support retraction emitted"
    );
    assert_eq!(frontier_of(&conn, "access"), 1);

    // 1 -> 0: the row retracts with exactly one net delete.
    conn.execute("DELETE FROM membership WHERE person = 1 AND team = 10", [])
        .unwrap();
    assert_snapshot(&conn, &program, "SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert!(weights(&conn, "access").is_empty());
    assert_eq!(
        delta(&conn, "access", 2),
        vec![(-1, vec![Cell::Integer(1), Cell::Integer(100)])]
    );
    assert_eq!(frontier_of(&conn, "access"), 2);

    program.teardown(&conn).unwrap();
}

/// Both join inputs pre-populated: bootstrap derives each matching pair once.
/// A later batch grows one input; a fanout-2 delete retracts both derivations
/// of the deleted row (the cross-term appears exactly once).
#[test]
fn join_both_inputs_prepopulated_then_cross_term_batch() {
    let conn = conn();
    conn.execute_batch(
        "INSERT INTO membership VALUES (1, 10), (2, 10);
         INSERT INTO permission VALUES (10, 100);",
    )
    .unwrap();
    let program = Program::install(&conn, "pairs", JOIN_SQL).unwrap();

    assert_eq!(
        weights(&conn, "pairs"),
        vec![
            (vec![Cell::Integer(1), Cell::Integer(100)], 1),
            (vec![Cell::Integer(2), Cell::Integer(100)], 1),
        ]
    );
    assert_snapshot(&conn, &program, "SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(frontier_of(&conn, "pairs"), 0);
    assert!(delta(&conn, "pairs", 2).is_empty());

    // One committed INSERT on the probe side: both derivations appear.
    conn.execute("INSERT INTO permission VALUES (10, 200)", [])
        .unwrap();
    assert_snapshot(&conn, &program, "SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(
        delta(&conn, "pairs", 2),
        vec![
            (1, vec![Cell::Integer(1), Cell::Integer(200)]),
            (1, vec![Cell::Integer(2), Cell::Integer(200)]),
        ]
    );
    assert_eq!(frontier_of(&conn, "pairs"), 1);

    // Fanout-2 delete: both derived rows lose their only support at once.
    conn.execute("DELETE FROM membership WHERE person = 1", [])
        .unwrap();
    assert_snapshot(&conn, &program, "SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(
        weights(&conn, "pairs"),
        vec![
            (vec![Cell::Integer(2), Cell::Integer(100)], 1),
            (vec![Cell::Integer(2), Cell::Integer(200)], 1),
        ]
    );
    assert_eq!(
        delta(&conn, "pairs", 2),
        vec![
            (-1, vec![Cell::Integer(1), Cell::Integer(100)]),
            (-1, vec![Cell::Integer(1), Cell::Integer(200)]),
        ]
    );
    assert_eq!(frontier_of(&conn, "pairs"), 2);

    program.teardown(&conn).unwrap();
}

/// A pre-populated group boots to the exact totals; a later UPDATE (the
/// collector's delete+insert pair) replaces the row under its key, and a
/// multi-statement transaction still bumps the frontier exactly once.
#[test]
fn group_prepopulated_then_replacement() {
    let conn = conn();
    conn.execute_batch("INSERT INTO job VALUES (1, 10, 5), (2, 10, 7), (3, 20, 11);")
        .unwrap();
    let program = Program::install(&conn, "team_total", TEAM_COST_SQL).unwrap();

    assert_eq!(
        group_state(&conn, "team_total"),
        vec![(10, 2, 12), (20, 1, 11)]
    );
    assert_snapshot(&conn, &program, "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team ORDER BY team");
    assert_eq!(frontier_of(&conn, "team_total"), 0);
    assert!(delta(&conn, "team_total", 3).is_empty());

    // Replacement: -old then +new under the same group key.
    conn.execute("UPDATE job SET cost = 9 WHERE id = 2", [])
        .unwrap();
    assert_snapshot(&conn, &program, "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team ORDER BY team");
    assert_eq!(
        group_state(&conn, "team_total"),
        vec![(10, 2, 14), (20, 1, 11)]
    );
    assert_eq!(
        delta(&conn, "team_total", 3),
        vec![
            (
                -1,
                vec![Cell::Integer(10), Cell::Integer(2), Cell::Integer(12)]
            ),
            (
                1,
                vec![Cell::Integer(10), Cell::Integer(2), Cell::Integer(14)]
            ),
        ]
    );
    assert_eq!(frontier_of(&conn, "team_total"), 1);

    // One transaction, two statements, one frontier, one net delta: the
    // (3,20,11) delete and the (4,20,11) insert cancel, so team 20's row is
    // untouched and only team 10's total changes.
    conn.execute_batch(
        "BEGIN;
         DELETE FROM job WHERE id = 3;
         INSERT INTO job VALUES (4, 20, 11), (5, 10, 11);
         COMMIT;",
    )
    .unwrap();
    assert_snapshot(&conn, &program, "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team ORDER BY team");
    assert_eq!(
        group_state(&conn, "team_total"),
        vec![(10, 3, 25), (20, 1, 11)]
    );
    assert_eq!(
        delta(&conn, "team_total", 3),
        vec![
            (
                -1,
                vec![Cell::Integer(10), Cell::Integer(2), Cell::Integer(14)]
            ),
            (
                1,
                vec![Cell::Integer(10), Cell::Integer(3), Cell::Integer(25)]
            ),
        ]
    );
    assert_eq!(frontier_of(&conn, "team_total"), 2);

    program.teardown(&conn).unwrap();
}

/// An install over empty sources stays at frontier 0 with an empty snapshot
/// and delta; the first committed write is frontier 1.
#[test]
fn empty_install_stays_at_frontier_zero() {
    let conn = conn();
    let program = Program::install(&conn, "access", ACCESS_SQL).unwrap();
    assert!(snapshot(&conn, &program).is_empty());
    assert!(delta(&conn, "access", 2).is_empty());
    assert_eq!(frontier_of(&conn, "access"), 0);

    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1, 10);
         INSERT INTO permission VALUES (10, 100);
         COMMIT;",
    )
    .unwrap();
    assert_snapshot(&conn, &program, "SELECT person, resource FROM direct_grant UNION SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team ORDER BY person, resource");
    assert_eq!(frontier_of(&conn, "access"), 1);
    assert_eq!(
        delta(&conn, "access", 2),
        vec![(1, vec![Cell::Integer(1), Cell::Integer(100)])]
    );
    program.teardown(&conn).unwrap();
}

/// Producer and consumer both installed over pre-populated tables: the
/// producer's view is seeded first, the consumer stages from the seeded view,
/// and one committed write settles both exactly once.
#[test]
fn prepopulated_producer_then_consumer_composition() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE body_a(person INTEGER PRIMARY KEY);
         CREATE TABLE body_b(person INTEGER PRIMARY KEY);
         CREATE TABLE body_c(person INTEGER PRIMARY KEY);
         CREATE TABLE grant_resource(person INTEGER PRIMARY KEY, resource INTEGER NOT NULL);
         INSERT INTO body_a VALUES (1);
         INSERT INTO body_b VALUES (1);
         INSERT INTO grant_resource VALUES (1, 10), (2, 20), (3, 30);",
    )
    .unwrap();

    const PRODUCER_SQL: &str =
        "SELECT person FROM body_a UNION SELECT person FROM body_b UNION SELECT person FROM body_c";
    const CONSUMER_SQL: &str =
        "SELECT v.person, g.resource FROM frontier_bodies v JOIN grant_resource g ON g.person = v.person";
    let composition =
        Composition::install(&conn, ("bodies", PRODUCER_SQL), ("grants", CONSUMER_SQL)).unwrap();

    // Producer support seeded with its duplicate weight; the consumer already
    // sees the seeded producer rows joined with the seeded grants.
    let support: Vec<(i64, i64)> = {
        let mut stmt = conn
            .prepare("SELECT person, __weight FROM frontier_bodies_root WHERE __weight > 0 ORDER BY person")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(support, vec![(1, 2)]);
    let visible: Vec<(i64, i64)> = {
        let mut stmt = conn
            .prepare("SELECT person, resource FROM frontier_grants ORDER BY person, resource")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(visible, vec![(1, 10)]);
    let consumer_delta: Vec<(i64, i64, i64)> = {
        let mut stmt = conn
            .prepare("SELECT __sign, person, resource FROM frontier_grants_delta")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert!(
        consumer_delta.is_empty(),
        "bootstrap emitted a consumer delta"
    );
    assert_eq!(frontier_of(&conn, "bodies"), 0);
    assert_eq!(frontier_of(&conn, "grants"), 0);
    assert_snapshot(
        &conn,
        &composition.consumer(),
        "SELECT v.person, g.resource FROM frontier_bodies v JOIN grant_resource g ON g.person = v.person ORDER BY v.person, g.resource",
    );

    // One COMMIT settles both: producer 0 -> 1, consumer 0 -> 1.
    conn.execute("INSERT INTO body_c VALUES (2)", []).unwrap();
    let support: Vec<(i64, i64)> = {
        let mut stmt = conn
            .prepare("SELECT person, __weight FROM frontier_bodies_root WHERE __weight > 0 ORDER BY person")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(support, vec![(1, 2), (2, 1)]);
    let visible: Vec<(i64, i64)> = {
        let mut stmt = conn
            .prepare("SELECT person, resource FROM frontier_grants ORDER BY person, resource")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(visible, vec![(1, 10), (2, 20)]);
    assert_eq!(frontier_of(&conn, "bodies"), 1);
    assert_eq!(frontier_of(&conn, "grants"), 1);

    composition.teardown(&conn).unwrap();
}

/// A failed bootstrap is atomic: no catalog row, no collector, no shadow
/// objects, and the source tables keep every pre-existing row — including
/// the cell that caused the rejection.
#[test]
fn failed_bootstrap_leaves_no_objects_and_sources_untouched() {
    // NULL in a staged row: explicit Unsupported, matching the settle-time
    // contract for watched sources.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE nullable_src(a INTEGER NOT NULL, b INTEGER);
         INSERT INTO nullable_src VALUES (1, 7), (2, NULL);",
    )
    .unwrap();
    assert_install_rejected(
        Program::install(&conn, "nullp", "SELECT a FROM nullable_src"),
        true,
        "NULL",
    );
    assert_no_program_objects(&conn);
    assert_eq!(catalog_rows(&conn, "nullp"), 0);
    let (rows, nulls): (i64, i64) = {
        let mut stmt = conn
            .prepare("SELECT count(*), count(b) FROM nullable_src")
            .unwrap();
        stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
    };
    assert_eq!((rows, nulls), (2, 1), "source rows were disturbed");
    conn.close().unwrap();

    // TEXT storage in a group-sum column would settle by coercion, not by
    // value: rejected instead of silently counted.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sum_src(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost);
         INSERT INTO sum_src VALUES (1, 10, 5), (2, 10, '7');",
    )
    .unwrap();
    assert_install_rejected(
        Program::install(
            &conn,
            "textsum",
            "SELECT team, count(*) AS jobs, sum(cost) AS total FROM sum_src GROUP BY team",
        ),
        true,
        "TEXT/BLOB",
    );
    assert_no_program_objects(&conn);
    assert_eq!(catalog_rows(&conn, "textsum"), 0);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM sum_src", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    conn.close().unwrap();

    // A SQL failure mid-bootstrap (integer overflow in the group sum) rolls
    // the whole install savepoint back the same way.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE big(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
         INSERT INTO big VALUES (1, 10, 9223372036854775807), (2, 10, 9223372036854775806);",
    )
    .unwrap();
    assert_install_rejected(
        Program::install(
            &conn,
            "overflow",
            "SELECT team, count(*) AS jobs, sum(cost) AS total FROM big GROUP BY team",
        ),
        false,
        "overflow",
    );
    assert_no_program_objects(&conn);
    assert_eq!(catalog_rows(&conn, "overflow"), 0);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM big", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    conn.close().unwrap();
}

/// Storage classes survive bootstrap exactly: no-affinity keys keep 5 and '5'
/// as distinct rows, REAL sums stay REAL, and the oracle agrees row for row.
#[test]
fn storage_classes_survive_bootstrap() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE mixed(k, w INTEGER NOT NULL);
         INSERT INTO mixed VALUES (5, 1), ('5', 1);
         CREATE TABLE jobr(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost REAL NOT NULL);
         INSERT INTO jobr VALUES (1, 10, 2.5), (2, 10, 1.0);",
    )
    .unwrap();

    let union = Program::install(&conn, "mixedk", "SELECT k FROM mixed").unwrap();
    assert_snapshot(&conn, &union, "SELECT k FROM mixed ORDER BY k");
    let rows = snapshot(&conn, &union);
    assert_eq!(
        rows,
        vec![vec![Cell::Integer(5)], vec![Cell::Text("5".into())],]
    );
    union.teardown(&conn).unwrap();

    let group = Program::install(
        &conn,
        "mixedg",
        "SELECT k, count(*) AS n, sum(w) AS s FROM mixed GROUP BY k",
    )
    .unwrap();
    assert_snapshot(
        &conn,
        &group,
        "SELECT k, count(*) AS n, sum(w) AS s FROM mixed GROUP BY k ORDER BY k",
    );
    group.teardown(&conn).unwrap();

    let real = Program::install(
        &conn,
        "jobreal",
        "SELECT team, count(*) AS jobs, sum(cost) AS total FROM jobr GROUP BY team",
    )
    .unwrap();
    let rows = snapshot(&conn, &real);
    assert_eq!(
        rows,
        vec![vec![Cell::Integer(10), Cell::Integer(2), Cell::Real(3.5),]]
    );
    assert_snapshot(
        &conn,
        &real,
        "SELECT team, count(*) AS jobs, sum(cost) AS total FROM jobr GROUP BY team ORDER BY team",
    );
    real.teardown(&conn).unwrap();
}
