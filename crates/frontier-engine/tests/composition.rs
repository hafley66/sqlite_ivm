//! Gate: one producer frontier view consumed by a second frontier view, both
//! settled inside one source transaction through a real SQLite connection.
//!
//! The producer (`job_pairs`) is a projection over `job`; the consumer
//! (`team_cost`) aggregates the producer's view. Every settled frontier
//! checks the consumer view against a fresh SQL recomputation over the base
//! table — insert, delete, mixed, rollback, and reopen — and the paired
//! stress case draws deterministic batches and recomputes after each one.
//!
//! Composed programs install without a commit collector, so settlement is
//! explicit: source rows land through SQL, then `Composition::settle` runs
//! both programs. The direct-batch contract from `cases.rs` applies: the
//! batch mirrors source changes that have already landed in the tables.

use frontier_engine::{Cell, Composition, Frontier, OutputChange, Program, Sign, SourceChange};
use rusqlite::Connection;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);",
    )
    .unwrap();
    conn
}

const PRODUCER_SQL: &str = "SELECT team, cost FROM job";
const CONSUMER_SQL: &str =
    "SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM frontier_job_pairs GROUP BY team";
const UNION_CONSUMER_SQL: &str = "SELECT team, cost FROM frontier_job_pairs";

fn install(conn: &Connection) -> Composition {
    Composition::install(conn, ("job_pairs", PRODUCER_SQL), ("team_cost", CONSUMER_SQL)).unwrap()
}

fn ins(id: i64, team: i64, cost: i64) -> SourceChange {
    SourceChange::insert("job", [Cell::Integer(id), Cell::Integer(team), Cell::Integer(cost)])
}

fn del(id: i64, team: i64, cost: i64) -> SourceChange {
    SourceChange::delete("job", [Cell::Integer(id), Cell::Integer(team), Cell::Integer(cost)])
}


/// The consumer view, straight SQL over the engine's visible output.
fn consumer_view(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT team, jobs, total_cost FROM frontier_team_cost ORDER BY team")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Fresh SQLite recomputation: the consumer's definition applied to the base
/// table, deduplicated exactly like the producer's set-union root.
fn oracle(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT team, count(*), sum(cost) FROM \
             (SELECT DISTINCT team, cost FROM job) GROUP BY team ORDER BY team",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[track_caller]
fn assert_tracks(conn: &Connection) {
    assert_eq!(
        consumer_view(conn),
        oracle(conn),
        "consumer view diverged from fresh recomputation"
    );
}

fn producer_delta(delta: &OutputChange) -> (i64, i64, i64) {
    let sign = match delta.sign {
        Sign::Insert => 1,
        Sign::Delete => -1,
    };
    match (&delta.row[0], &delta.row[1]) {
        (Cell::Integer(team), Cell::Integer(cost)) => (sign, *team, *cost),
        _ => panic!("non-integer producer output"),
    }
}

fn consumer_delta(delta: &OutputChange) -> (i64, i64, i64, i64) {
    let sign = match delta.sign {
        Sign::Insert => 1,
        Sign::Delete => -1,
    };
    match (&delta.row[0], &delta.row[1], &delta.row[2]) {
        (Cell::Integer(team), Cell::Integer(jobs), Cell::Integer(total)) => {
            (sign, *team, *jobs, *total)
        }
        _ => panic!("non-integer consumer output"),
    }
}

#[test]
fn composed_pair_settles_in_one_source_transaction() {
    let conn = conn();
    let composed = install(&conn);

    // An empty batch is a valid frontier over both programs.
    let delta = composed.settle(&conn, &[]).unwrap();
    assert!(delta.producer.is_empty() && delta.consumer.is_empty());
    assert_tracks(&conn);

    // Insert batch: the producer emits the new rows, the consumer the groups.
    conn.execute_batch("BEGIN; INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11); COMMIT;")
        .unwrap();
    let delta = composed
        .settle(&conn, &[ins(1, 10, 5), ins(2, 10, 7), ins(3, 20, 11)])
        .unwrap();
    let mut producer: Vec<(i64, i64, i64)> = delta.producer.iter().map(producer_delta).collect();
    producer.sort();
    assert_eq!(producer, vec![(1, 10, 5), (1, 10, 7), (1, 20, 11)]);
    let mut consumer: Vec<(i64, i64, i64, i64)> = delta.consumer.iter().map(consumer_delta).collect();
    consumer.sort();
    assert_eq!(consumer, vec![(1, 10, 2, 12), (1, 20, 1, 11)]);
    assert_eq!(consumer_view(&conn), vec![(10, 2, 12), (20, 1, 11)]);
    assert_tracks(&conn);
    // Delete batch: full retraction of one producer row and its support.
    conn.execute_batch("DELETE FROM job WHERE id = 2;").unwrap();
    let delta = composed.settle(&conn, &[del(2, 10, 7)]).unwrap();
    assert_eq!(
        delta.producer.iter().map(producer_delta).collect::<Vec<_>>(),
        vec![(-1, 10, 7)]
    );
    // The group root emits changed groups as before/after image pairs.
    assert_eq!(
        delta.consumer.iter().map(consumer_delta).collect::<Vec<_>>(),
        vec![(1, 10, 1, 5), (-1, 10, 2, 12)]
    );
    assert_eq!(consumer_view(&conn), vec![(10, 1, 5), (20, 1, 11)]);
    assert_tracks(&conn);

    // Mixed batch: one row replaced net-zero inside a group, one group added.
    conn.execute_batch(
        "BEGIN;
         DELETE FROM job WHERE id = 1;
         INSERT INTO job VALUES (5,10,5);
         INSERT INTO job VALUES (4,30,9);
         COMMIT;",
    )
    .unwrap();
    let delta = composed
        .settle(&conn, &[del(1, 10, 5), ins(5, 10, 5), ins(4, 30, 9)])
        .unwrap();
    assert_eq!(
        delta.producer.iter().map(producer_delta).collect::<Vec<_>>(),
        vec![(1, 30, 9)],
        "the net-zero row change must not reach either delta"
    );
    assert_eq!(
        delta.consumer.iter().map(consumer_delta).collect::<Vec<_>>(),
        vec![(1, 30, 1, 9)]
    );
    assert_eq!(consumer_view(&conn), vec![(10, 1, 5), (20, 1, 11), (30, 1, 9)]);
    assert_tracks(&conn);

    assert_eq!(composed.producer().frontier_id(&conn).unwrap(), 4);
    assert_eq!(composed.consumer().frontier_id(&conn).unwrap(), 4);
}

#[test]
fn rollback_undoes_both_programs_to_the_previous_frontier() {
    let conn = conn();
    let composed = install(&conn);

    conn.execute_batch("INSERT INTO job VALUES (1,10,5),(2,20,7);")
        .unwrap();
    composed.settle(&conn, &[ins(1, 10, 5), ins(2, 20, 7)]).unwrap();
    let before = consumer_view(&conn);
    let producer_frontier = composed.producer().frontier_id(&conn).unwrap();
    let consumer_frontier = composed.consumer().frontier_id(&conn).unwrap();

    // One source transaction rolls the whole composed frontier back.
    conn.execute_batch("BEGIN; INSERT INTO job VALUES (3,10,6);").unwrap();
    composed.settle(&conn, &[ins(3, 10, 6)]).unwrap();
    assert_eq!(consumer_view(&conn), vec![(10, 2, 11), (20, 1, 7)]);
    conn.execute_batch("ROLLBACK;").unwrap();

    assert_eq!(consumer_view(&conn), before);
    assert_tracks(&conn);
    assert_eq!(composed.producer().frontier_id(&conn).unwrap(), producer_frontier);
    assert_eq!(composed.consumer().frontier_id(&conn).unwrap(), consumer_frontier);
}

#[test]
fn reopen_continues_the_chain_from_the_catalog() {
    let conn = conn();
    {
        let composed = install(&conn);
        conn.execute_batch("INSERT INTO job VALUES (1,10,5);").unwrap();
        composed.settle(&conn, &[ins(1, 10, 5)]).unwrap();
    } // handles dropped: the pair lives only in the connection.

    let composed = Composition::open(&conn, "job_pairs", "team_cost").unwrap();
    assert_tracks(&conn);

    conn.execute_batch("INSERT INTO job VALUES (2,10,7),(3,20,11);")
        .unwrap();
    composed
        .settle(&conn, &[ins(2, 10, 7), ins(3, 20, 11)])
        .unwrap();
    assert_tracks(&conn);
    assert_eq!(consumer_view(&conn), vec![(10, 2, 12), (20, 1, 11)]);
    // The frontier counters continued across the reopen, one bump per batch.
    assert_eq!(composed.producer().frontier_id(&conn).unwrap(), 2);
    assert_eq!(composed.consumer().frontier_id(&conn).unwrap(), 2);
}

#[test]
fn sql_writes_alone_do_not_settle_composed_programs() {
    let conn = conn();
    let composed = install(&conn);

    // No collector: an autocommit write leaves both views untouched until the
    // caller settles. That is the explicit contract, not a lost update.
    conn.execute_batch("INSERT INTO job VALUES (1,10,5);").unwrap();
    assert!(consumer_view(&conn).is_empty());
    assert_eq!(composed.producer().frontier_id(&conn).unwrap(), 0);
    assert_eq!(composed.consumer().frontier_id(&conn).unwrap(), 0);

    composed.settle(&conn, &[ins(1, 10, 5)]).unwrap();
    assert_tracks(&conn);
    assert_eq!(consumer_view(&conn), vec![(10, 1, 5)]);
}

#[test]
fn union_root_consumer_over_producer_view() {
    let conn = conn();
    let composed =
        Composition::install(&conn, ("job_pairs", PRODUCER_SQL), ("pairs_view", UNION_CONSUMER_SQL))
            .unwrap();

    conn.execute_batch("INSERT INTO job VALUES (1,10,5),(2,10,5),(3,20,11);")
        .unwrap();
    composed
        .settle(&conn, &[ins(1, 10, 5), ins(2, 10, 5), ins(3, 20, 11)])
        .unwrap();

    // The consumer's union root sees exactly the producer's visible rows,
    // duplicate support collapsed the same way.
    let mut stmt = conn
        .prepare("SELECT team, cost FROM frontier_pairs_view ORDER BY team, cost")
        .unwrap();
    let view: Vec<(i64, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(view, vec![(10, 5), (20, 11)]);
}

#[test]
fn unsupported_compositions_are_explicit() {
    let conn = conn();
    let _composed = install(&conn);

    // A watched program over a program view: rejected with the composition
    // pointer, never a trigger DDL error.
    let err = Program::install(&conn, "sneaky", "SELECT team FROM frontier_job_pairs")
        .unwrap_err();
    assert!(err.is_unsupported());
    assert_eq!(err.relation, "frontier_job_pairs");

    // A consumer over a plain table is not a composition edge.
    let err = Composition::install(
        &conn,
        ("job_pairs2", PRODUCER_SQL),
        ("plain", "SELECT team, count(*) AS n FROM job GROUP BY team"),
    )
    .unwrap_err();
    assert!(err.is_unsupported());

    // A producer reading another program's view would be a second level.
    let above_sql = CONSUMER_SQL.replace("frontier_job_pairs", "frontier_chained");
    let err = Composition::install(
        &conn,
        ("chained", "SELECT team FROM frontier_job_pairs"),
        ("above", above_sql.as_str()),
    )
    .unwrap_err();
    assert!(err.is_unsupported());
    assert_eq!(err.relation, "frontier_job_pairs");

    // The rejected installs left no objects behind.
    let leftovers: i64 = conn
        .query_row(
            "SELECT count(*) FROM frontier_catalog WHERE name IN ('sneaky','job_pairs2','plain','chained','above')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
}

#[test]
fn paired_stress_composed_pair_tracks_fresh_recomputation() {
    fn draw(state: &mut u64, modulus: u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*state >> 33) % modulus
    }

    let conn = conn();
    let composed = install(&conn);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut live: Vec<(i64, i64, i64)> = Vec::new();
    let mut id_seq: i64 = 0;

    for _ in 0..40 {
        let mut batch: Vec<SourceChange> = Vec::new();
        conn.execute_batch("BEGIN;").unwrap();
        for _ in 0..1 + draw(&mut state, 6) {
            if live.len() > 3 && draw(&mut state, 100) < 45 {
                let at = draw(&mut state, live.len() as u64) as usize;
                let (id, team, cost) = live.remove(at);
                conn.execute("DELETE FROM job WHERE id = ?1", [id]).unwrap();
                batch.push(del(id, team, cost));
            } else {
                id_seq += 1;
                let team = 1 + draw(&mut state, 4) as i64;
                let cost = 1 + draw(&mut state, 50) as i64;
                conn.execute(
                    "INSERT INTO job VALUES (?1, ?2, ?3)",
                    [id_seq, team, cost],
                )
                .unwrap();
                batch.push(ins(id_seq, team, cost));
            }
        }
        composed.settle(&conn, &batch).unwrap();
        conn.execute_batch("COMMIT;").unwrap();
        assert_tracks(&conn);
    }

    // The randomized walk may drain the table; the frontier counter still
    // advanced once per settled batch, and every one of them tracked.
    assert_eq!(composed.producer().frontier_id(&conn).unwrap(), 40);
    assert_eq!(composed.consumer().frontier_id(&conn).unwrap(), 40);
}
