//! Runnable walkthrough of both packet cases over one in-memory connection.
//!
//! Self-asserting: every frontier checks its required net output change and
//! visible snapshot. Run with `cargo run --offline --example cases`.

use ivm_sqlite::{Cell, Frontier, Program};
use rusqlite::Connection;

fn setup() -> Connection {
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

fn pairs(program: &Program, conn: &Connection) -> Vec<(i64, i64)> {
    let mut rows: Vec<(i64, i64)> = program
        .snapshot(conn)
        .unwrap()
        .into_iter()
        .map(|t| match (&t.0[0], &t.0[1]) {
            (Cell::Integer(a), Cell::Integer(b)) => (*a, *b),
            _ => panic!("non-integer output"),
        })
        .collect();
    rows.sort_unstable();
    rows
}

fn groups(program: &Program, conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut rows: Vec<(i64, i64, i64)> = program
        .snapshot(conn)
        .unwrap()
        .into_iter()
        .map(|t| match (&t.0[0], &t.0[1], &t.0[2]) {
            (Cell::Integer(a), Cell::Integer(b), Cell::Integer(c)) => (*a, *b, *c),
            _ => panic!("non-integer output"),
        })
        .collect();
    rows.sort_unstable();
    rows
}

#[track_caller]
fn expect(got: Vec<(i64, i64)>, want: &[&str]) {
    let want: Vec<(i64, i64)> = want
        .iter()
        .map(|s| s.split_once(',').unwrap())
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .collect();
    assert_eq!(got, want, "visible output mismatch");
}

fn main() {
    let conn = setup();
    let access = Program::install(
        &conn,
        "access",
        "SELECT person, resource FROM direct_grant \
         UNION \
         SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team",
    )
    .unwrap();

    // Frontier 0: two memberships, two permissions, one direct grant.
    conn.execute_batch(
        "BEGIN;
         INSERT INTO membership VALUES (1,10),(1,20);
         INSERT INTO permission VALUES (10,100),(20,100);
         INSERT INTO direct_grant VALUES (3,300);
         COMMIT;",
    )
    .unwrap();
    expect(pairs(&access, &conn), &["1,100", "3,300"]);

    // Frontier 1: the join cross-term (2,200) appears exactly once.
    conn.execute_batch("BEGIN; INSERT INTO membership VALUES (2,10); INSERT INTO permission VALUES (10,200); COMMIT;").unwrap();
    expect(
        pairs(&access, &conn),
        &["1,100", "1,200", "2,100", "2,200", "3,300"],
    );

    // Frontier 2: duplicate union support (2 -> 1) changes nothing.
    conn.execute_batch("INSERT INTO direct_grant VALUES (1,200);")
        .unwrap();
    expect(
        pairs(&access, &conn),
        &["1,100", "1,200", "2,100", "2,200", "3,300"],
    );

    // Frontier 3: join support retraction (2 -> 1) changes nothing.
    conn.execute_batch("DELETE FROM membership WHERE person=1 AND team=10;")
        .unwrap();
    expect(
        pairs(&access, &conn),
        &["1,100", "1,200", "2,100", "2,200", "3,300"],
    );

    // Frontier 4: last join support (1 -> 0) retracts.
    conn.execute_batch("DELETE FROM permission WHERE team=20 AND resource=100;")
        .unwrap();
    expect(pairs(&access, &conn), &["1,200", "2,100", "2,200", "3,300"]);

    // Frontier 5: last union support retracts.
    conn.execute_batch("DELETE FROM direct_grant WHERE person=1 AND resource=200;")
        .unwrap();
    expect(pairs(&access, &conn), &["2,100", "2,200", "3,300"]);

    // Frontier 6: a savepoint rollback leaves no committed trace.
    let before = access.frontier_id(&conn).unwrap();
    conn.execute_batch("BEGIN; SAVEPOINT d; INSERT INTO direct_grant VALUES (4,400); ROLLBACK TO d; RELEASE d; COMMIT;").unwrap();
    expect(pairs(&access, &conn), &["2,100", "2,200", "3,300"]);
    assert_eq!(access.frontier_id(&conn).unwrap(), before);

    // Frontier 7: a transaction rollback leaves no committed trace.
    conn.execute_batch("BEGIN; INSERT INTO membership VALUES (5,10); ROLLBACK;")
        .unwrap();
    expect(pairs(&access, &conn), &["2,100", "2,200", "3,300"]);
    assert_eq!(access.frontier_id(&conn).unwrap(), before);

    // Frontier 8: an update moves the output row.
    conn.execute_batch("UPDATE permission SET resource=300 WHERE team=10 AND resource=200;")
        .unwrap();
    expect(pairs(&access, &conn), &["2,100", "2,300", "3,300"]);

    // The aggregate case: same trait object shape, no signature change.
    let team_cost = Program::install(
        &conn,
        "team_cost",
        "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team",
    )
    .unwrap();

    conn.execute_batch("BEGIN; INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11); COMMIT;")
        .unwrap();
    assert_eq!(groups(&team_cost, &conn), vec![(10, 2, 12), (20, 1, 11)]);

    // Move a job between teams and add one, in a single frontier.
    conn.execute_batch(
        "BEGIN; INSERT INTO job VALUES (4,10,3); UPDATE job SET team=10 WHERE id=3; COMMIT;",
    )
    .unwrap();
    assert_eq!(groups(&team_cost, &conn), vec![(10, 4, 26)]);

    // A cost moves across zero.
    conn.execute_batch("UPDATE job SET cost=-7 WHERE id=2;")
        .unwrap();
    assert_eq!(groups(&team_cost, &conn), vec![(10, 4, 12)]);

    // Deleting every job leaves zero snapshot rows, and the delta records it.
    conn.execute_batch("BEGIN; DELETE FROM job WHERE id IN (1,4); COMMIT;")
        .unwrap();
    assert_eq!(groups(&team_cost, &conn), vec![(10, 2, 4)]);
    conn.execute_batch("DELETE FROM job WHERE id IN (2,3);")
        .unwrap();
    assert!(groups(&team_cost, &conn).is_empty());

    println!(
        "all 15 frontiers settled as required; final frontier ids: access={}, team_cost={}",
        access.frontier_id(&conn).unwrap(),
        team_cost.frontier_id(&conn).unwrap()
    );
}
