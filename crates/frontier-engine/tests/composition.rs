//! Gate: ordinary SQL INSERT/DELETE/UPDATE + COMMIT on the base tables
//! settles producer AND consumer at the same COMMIT through one shared,
//! graph-owned collector. No manual settle exists: `Composition` exposes
//! only install/open/teardown, and the tests drive real SQL only.
//!
//! The oracle pair mirrors `plans/engine-iso/8_composed_oracle.sql` +
//! `9_composed_expected.tsv`: the producer is the set-union of three body
//! tables (body_c starts empty, so "add a body" is an INSERT), the consumer
//! joins the producer's view with `grant_resource`. Every step checks the
//! support weights, the consumer view, and the consumer's settled delta —
//! including the support 3->2 step that must emit no downstream change, the
//! one-transaction change of both consumer join inputs, and a rolled-back
//! transaction.
//!
//! `vtab_write_inside_xsync_is_rejected` reproduces the SQLite behavior that
//! forces the shared-collector design (see `src/composition.rs` module docs):
//! a virtual-table write fired inside another collector's `xSync` fails the
//! COMMIT with SQLITE_LOCKED. If SQLite ever stops rejecting that, this test
//! fails and the design gets reevaluated.
//!
//! `INSERT OR REPLACE` caveat: with SQLite's default `recursive_triggers =
//! OFF`, the DELETE that REPLACE performs to clear a conflicting row fires
//! no AFTER DELETE trigger, so the collector sees only the inserted half
//! and a retracted row would stay visible. `INSERT`/`DELETE`/`UPDATE` are
//! the supported write surface; enabling `PRAGMA recursive_triggers` on the
//! connection restores correct REPLACE handling.

use frontier_engine::{Composition, Frontier, Program};
use rusqlite::Connection;
use sqlite_ext::{watch, BulkTrigger, RowChange};

fn create_base_tables(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE body_a(person INTEGER PRIMARY KEY);
         CREATE TABLE body_b(person INTEGER PRIMARY KEY);
         CREATE TABLE body_c(person INTEGER PRIMARY KEY);
         CREATE TABLE grant_resource(person INTEGER PRIMARY KEY, resource INTEGER NOT NULL);",
    )
    .unwrap();
}

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    create_base_tables(&conn);
    conn
}

const PRODUCER_SQL: &str =
    "SELECT person FROM body_a UNION SELECT person FROM body_b UNION SELECT person FROM body_c";
const CONSUMER_SQL: &str =
    "SELECT v.person, g.resource FROM frontier_bodies v JOIN grant_resource g ON g.person = v.person";

fn install(conn: &Connection) -> Composition {
    Composition::install(conn, ("bodies", PRODUCER_SQL), ("grants", CONSUMER_SQL)).unwrap()
}

#[test]
fn composition_dependency_is_persisted_and_removed() {
    let conn = conn();
    let pair = install(&conn);
    let producer: String = conn
        .query_row(
            "SELECT producer FROM frontier_dependency WHERE consumer='grants'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(producer, "bodies");
    pair.teardown(&conn).unwrap();
    let edges: i64 = conn
        .query_row("SELECT count(*) FROM frontier_dependency", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(edges, 0);
}

/// Seeds the oracle's 0_initial state in one frontier: Alice supported twice,
/// body_c empty, grants for persons 1 and 2.
fn seed(conn: &Connection) {
    conn.execute_batch(
        "BEGIN;
         INSERT INTO body_a VALUES (1);
         INSERT INTO body_b VALUES (1);
         INSERT INTO grant_resource VALUES (1,10),(2,20),(3,30);
         COMMIT;",
    )
    .unwrap();
}

/// The producer's support: visible output rows with their derivation weights.
fn support(conn: &Connection) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT person, __weight FROM frontier_bodies_root WHERE __weight > 0 ORDER BY person",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The consumer view.
fn visible(conn: &Connection) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT person, resource FROM frontier_grants ORDER BY person, resource")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The consumer's settled delta, in the engine's read order.
fn grants_delta(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT person, resource, __sign FROM frontier_grants_delta ORDER BY person, resource, __sign")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn frontier_of(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT frontier FROM frontier_catalog WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[track_caller]
fn assert_support(conn: &Connection, expected: &[(i64, i64)]) {
    assert_eq!(support(conn), expected, "support diverged");
}

#[track_caller]
fn assert_visible(conn: &Connection, expected: &[(i64, i64)]) {
    assert_eq!(visible(conn), expected, "consumer view diverged");
}

/// The oracle sequence from plans/engine-iso 8_composed_oracle.sql /
/// 9_composed_expected.tsv, expressed as plain SQL COMMITs.
#[test]
fn composed_oracle_from_plans_engine_iso() {
    let conn = conn();
    let composed = install(&conn);
    seed(&conn);

    // 0_initial: Alice support 2, downstream (1,10) already settled.
    assert_support(&conn, &[(1, 2)]);
    assert_visible(&conn, &[(1, 10)]);
    assert_eq!(grants_delta(&conn), vec![(1, 10, 1)]);

    // 1_add_body_c: Bob's first support, only Bob changes downstream.
    conn.execute_batch("INSERT INTO body_c VALUES (1),(2);")
        .unwrap();
    assert_support(&conn, &[(1, 3), (2, 1)]);
    assert_visible(&conn, &[(1, 10), (2, 20)]);
    assert_eq!(grants_delta(&conn), vec![(2, 20, 1)]);

    // 2_remove_body_b: Alice 3->2, visibility unchanged, no downstream delta.
    conn.execute_batch("DELETE FROM body_b;").unwrap();
    assert_support(&conn, &[(1, 2), (2, 1)]);
    assert_visible(&conn, &[(1, 10), (2, 20)]);
    assert_eq!(grants_delta(&conn), Vec::new());

    // 3_remove_body_c: Bob retracts, Alice keeps one support through A.
    conn.execute_batch("DELETE FROM body_c;").unwrap();
    assert_support(&conn, &[(1, 1)]);
    assert_visible(&conn, &[(1, 10)]);
    assert_eq!(grants_delta(&conn), vec![(2, 20, -1)]);

    // 4_both_inputs: the union AND the consumer's other join input change in
    // one transaction; the net downstream change is exactly +(2,21).
    conn.execute_batch(
        "BEGIN;
         INSERT INTO body_a VALUES (2);
         UPDATE grant_resource SET resource = 21 WHERE person = 2;
         COMMIT;",
    )
    .unwrap();
    assert_support(&conn, &[(1, 1), (2, 1)]);
    assert_visible(&conn, &[(1, 10), (2, 21)]);
    assert_eq!(grants_delta(&conn), vec![(2, 21, 1)]);

    // 5_rollback: a discarded transaction leaves graph and rows unchanged.
    let (support_before, visible_before) = (support(&conn), visible(&conn));
    conn.execute_batch(
        "BEGIN;
         DELETE FROM body_a WHERE person = 1;
         INSERT INTO body_c VALUES (9);
         UPDATE grant_resource SET resource = 99 WHERE person = 1;
         ROLLBACK;",
    )
    .unwrap();
    assert_eq!(support(&conn), support_before);
    assert_eq!(visible(&conn), visible_before);
    assert_eq!(
        grants_delta(&conn),
        vec![(2, 21, 1)],
        "the delta table keeps the last settled frontier"
    );

    composed.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}
/// One COMMIT advances both programs exactly once: after a single INSERT the
/// producer view and the consumer view are both already updated, and each
/// catalog frontier moved by exactly one (no double settle).
#[test]
fn one_commit_settles_both_programs() {
    let conn = conn();
    let composed = install(&conn);
    seed(&conn);
    assert_eq!(frontier_of(&conn, "bodies"), 1);
    assert_eq!(frontier_of(&conn, "grants"), 1);

    conn.execute_batch("INSERT INTO body_c VALUES (1);")
        .unwrap();

    assert_eq!(frontier_of(&conn, "bodies"), 2, "producer settled once");
    assert_eq!(frontier_of(&conn, "grants"), 2, "consumer settled once");
    assert_support(&conn, &[(1, 3)]);
    assert_visible(&conn, &[(1, 10)]);

    composed.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}

/// A settle that fails inside the shared collector's xSync fails the whole
/// COMMIT and leaves the previous committed state everywhere; the next
/// ordinary write still settles.
#[test]
fn settle_failure_inside_commit_aborts_everything() {
    let conn = conn();
    conn.execute_batch(
        "CREATE TABLE kv(k INTEGER PRIMARY KEY, grp INTEGER NOT NULL, v INTEGER NOT NULL);",
    )
    .unwrap();
    let composed = Composition::install(
        &conn,
        ("pairs", "SELECT grp, v FROM kv"),
        (
            "totals",
            "SELECT grp, count(*) AS n, sum(v) AS total FROM frontier_pairs GROUP BY grp",
        ),
    )
    .unwrap();
    conn.execute_batch("INSERT INTO kv VALUES (1, 10, 100);")
        .unwrap();
    assert_eq!(visible_totals(&conn), vec![(10, 1, 100)]);

    // Two new pairs in one settle: the consumer's stage sum overflows, the
    // producer settles, the consumer errors, and SQLite rolls the whole
    // transaction back. (A single +MAX row would not error: the + operator
    // silently promotes to float, only sum() over the stage errors.)
    let commit = conn.execute_batch(
        "BEGIN;
         INSERT INTO kv VALUES (2, 10, 9223372036854775807);
         INSERT INTO kv VALUES (3, 10, 5);
         COMMIT;",
    );
    assert!(
        commit.is_err(),
        "overflowing consumer settle must fail the COMMIT"
    );
    let _ = conn.execute_batch("ROLLBACK;");
    let kv_rows: i64 = conn
        .query_row("SELECT count(*) FROM kv", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kv_rows, 1, "the failed transaction left no rows behind");
    assert_eq!(visible_totals(&conn), vec![(10, 1, 100)]);

    // The collector survives the failed commit and settles the next one.
    conn.execute_batch("INSERT INTO kv VALUES (3, 10, 1);")
        .unwrap();
    assert_eq!(visible_totals(&conn), vec![(10, 2, 101)]);

    composed.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}

fn visible_totals(conn: &Connection) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT grp, n, total FROM frontier_totals ORDER BY grp")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Same-connection handle reopen: dropping the handles without teardown
/// leaves the schema objects; `Composition::open` re-registers the shared
/// collector and ordinary SQL keeps settling, with the frontier counters
/// continuing from where they were. This is NOT process-restart behavior:
/// a new connection cannot reattach the collector module at all — see
/// `fresh_connection_reattaches_the_shared_collector`.
#[test]
fn same_connection_reopen_reregisters_the_shared_collector() {
    let conn = conn();
    {
        let composed = install(&conn);
        seed(&conn);
        assert_eq!(frontier_of(&conn, "bodies"), 1);
        drop(composed);
    }

    let reopened = Composition::open(&conn, "bodies", "grants").unwrap();
    conn.execute_batch("INSERT INTO body_c VALUES (3);")
        .unwrap();
    assert_eq!(
        frontier_of(&conn, "bodies"),
        2,
        "counter continues across reopen"
    );
    assert_eq!(frontier_of(&conn, "grants"), 2);
    assert_support(&conn, &[(1, 2), (3, 1)]);
    assert_visible(&conn, &[(1, 10), (3, 30)]);

    reopened.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}

/// A new connection restores the graph-owned collector before writing any
/// source table. The producer and consumer continue from persisted roots and
/// frontier counters; no source recomputation or collector replacement occurs.
#[test]
fn fresh_connection_reattaches_the_shared_collector() {
    let db = TempDb::new("frontier-composition-reopen");
    {
        let conn = Connection::open(&db.path).unwrap();
        create_base_tables(&conn);
        let composed = install(&conn);
        seed(&conn);
        conn.execute_batch("INSERT INTO body_c VALUES (3);")
            .unwrap();
        assert_visible(&conn, &[(1, 10), (3, 30)]);
        drop(composed);
    }

    let conn = Connection::open(&db.path).unwrap();
    let reopened = Composition::reattach(&conn, "bodies", "grants").unwrap();
    assert_eq!(frontier_of(&conn, "bodies"), 2);
    assert_eq!(frontier_of(&conn, "grants"), 2);
    assert_visible(&conn, &[(1, 10), (3, 30)]);
    conn.execute_batch("INSERT INTO body_c VALUES (2);")
        .unwrap();
    assert_eq!(frontier_of(&conn, "bodies"), 3);
    assert_eq!(frontier_of(&conn, "grants"), 3);
    assert_visible(&conn, &[(1, 10), (2, 20), (3, 30)]);
    reopened.teardown(&conn).unwrap();
}

/// A unique file-backed database, removed when the test ends.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "{name}-{}-{}.sqlite",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The measured SQLite behavior that dictates the shared collector: a
/// virtual-table write fired inside another collector's xSync is rejected
/// with SQLITE_LOCKED and the whole transaction rolls back. See the
/// `src/composition.rs` module docs.
#[test]
fn vtab_write_inside_xsync_is_rejected() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE watched(v INTEGER);
         CREATE TABLE side_effect(v INTEGER);",
    )
    .unwrap();
    watch(&db, "col_writer", &["watched"], WritesOtherTable).unwrap();
    watch(&db, "col_sink", &["side_effect"], Absorbs).unwrap();

    db.execute_batch("BEGIN; INSERT INTO watched VALUES (1);")
        .unwrap();
    let commit = db.execute_batch("COMMIT;");
    let err = commit
        .err()
        .expect("COMMIT must fail: a vtab write inside another xSync is rejected");
    match &err {
        rusqlite::Error::SqliteFailure(e, _) => {
            assert_eq!(
                e.extended_code,
                rusqlite::ffi::SQLITE_LOCKED,
                "expected SQLITE_LOCKED, got {err}"
            );
        }
        other => panic!("expected SqliteFailure, got {other:?}"),
    }
    let rolled_back: i64 = db
        .query_row("SELECT count(*) FROM watched", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        rolled_back, 0,
        "the failed COMMIT rolled the transaction back"
    );
}

struct WritesOtherTable;

impl BulkTrigger for WritesOtherTable {
    fn on_batch(&mut self, db: &Connection, _batch: &[RowChange]) -> rusqlite::Result<()> {
        db.execute("INSERT INTO side_effect VALUES (1)", [])?;
        Ok(())
    }
}

struct Absorbs;

impl BulkTrigger for Absorbs {
    fn on_batch(&mut self, _db: &Connection, _batch: &[RowChange]) -> rusqlite::Result<()> {
        Ok(())
    }
}

/// Every unsupported composition shape fails as explicit `Unsupported` at
/// install, and a failed install leaves no objects behind.
#[test]
fn unsupported_compositions_are_explicit() {
    let conn = conn();
    let composed = install(&conn);

    // A plain watched program over a program view.
    let sneaky = Program::install(&conn, "sneaky", "SELECT person FROM frontier_bodies");
    assert_unsupported(sneaky, "not a settlement source");

    // The producer of a composition reading a program view.
    let bad_producer = Composition::install(
        &conn,
        ("bad", "SELECT person FROM frontier_bodies"),
        ("orphan", "SELECT person FROM grant_resource"),
    );
    assert_unsupported(bad_producer, "base tables only");

    // A consumer settling from two program views.
    let other = Program::install(&conn, "other", "SELECT person FROM body_c").unwrap();
    let two_views = Composition::install(
        &conn,
        ("p2", "SELECT person FROM body_a"),
        (
            "c2",
            "SELECT person FROM frontier_p2 UNION SELECT person FROM frontier_other",
        ),
    );
    assert_unsupported(two_views, "at most one producer");

    // A consumer that never reads the producer's view.
    let no_view = Composition::install(
        &conn,
        ("p3", "SELECT person FROM body_a"),
        ("c3", "SELECT person FROM grant_resource"),
    );
    assert_unsupported(no_view, "must read the producer's output view");

    // A consumer join input that is a plain view, not a base table.
    conn.execute_batch("CREATE VIEW helper AS SELECT person, resource FROM grant_resource;")
        .unwrap();
    let view_input = Composition::install(
        &conn,
        ("p4", "SELECT person FROM body_a"),
        (
            "c4",
            "SELECT v.person, h.resource FROM frontier_p4 v JOIN helper h ON h.person = v.person",
        ),
    );
    assert_unsupported(view_input, "only base tables can be watched");

    composed.teardown(&conn).unwrap();
    other.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}

#[track_caller]
fn assert_unsupported(
    result: Result<impl std::any::Any, frontier_engine::EngineError>,
    needle: &str,
) {
    let err = result.err().expect("install must be rejected");
    let text = format!("{err}");
    assert!(err.is_unsupported(), "not Unsupported: {text}");
    assert!(text.contains(needle), "message '{text}' lacks '{needle}'");
}

#[track_caller]
fn assert_no_program_objects(conn: &Connection) {
    let left: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'frontier_%' \
             AND name NOT IN ('frontier_catalog', 'frontier_catalog_column', 'frontier_dependency')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "program objects leaked");
}

/// 40 deterministic randomized transactions over all four base tables; after
/// each COMMIT both views must equal a fresh SQL recomputation over the base
/// tables.
#[test]
fn stress_tracks_fresh_recomputation() {
    let conn = conn();
    let composed = install(&conn);
    seed(&conn);

    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as i64
    };
    for step in 0..40 {
        let mut sql = String::from("BEGIN;");
        for _ in 0..1 + next() % 3 {
            match next() % 4 {
                0 => {
                    let table = ['a', 'b', 'c'][(next() % 3) as usize];
                    let person = 1 + next() % 6;
                    sql.push_str(&format!(
                        "INSERT INTO body_{table} SELECT {person} WHERE NOT EXISTS \
                         (SELECT 1 FROM body_{table} WHERE person = {person});"
                    ));
                }
                1 => sql.push_str(&format!(
                    "DELETE FROM body_{} WHERE person = {};",
                    ['a', 'b', 'c'][(next() % 3) as usize],
                    1 + next() % 6
                )),
                2 => sql.push_str(&format!(
                    "UPDATE grant_resource SET resource = {} WHERE person = {};",
                    10 * (1 + next() % 6),
                    1 + next() % 6
                )),
                _ => {
                    let person = 1 + next() % 6;
                    let resource = 10 * (1 + next() % 6);
                    sql.push_str(&format!(
                        "INSERT INTO grant_resource SELECT {person}, {resource} WHERE NOT EXISTS \
                         (SELECT 1 FROM grant_resource WHERE person = {person});"
                    ));
                }
            };
        }
        sql.push_str("COMMIT;");
        conn.execute_batch(&sql).unwrap();

        let after = format!("after `{sql}`");

        assert_eq!(
            visible(&conn),
            recomputed_visible(&conn),
            "step {step} {after}: consumer view diverged from fresh recomputation"
        );
        assert_eq!(
            support(&conn),
            recomputed_support(&conn),
            "step {step} {after}: support diverged from fresh recomputation"
        );
    }

    composed.teardown(&conn).unwrap();
    assert_no_program_objects(&conn);
}

fn recomputed_support(conn: &Connection) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT person, count(*) FROM \
             (SELECT person FROM body_a UNION ALL SELECT person FROM body_b UNION ALL SELECT person FROM body_c) \
             GROUP BY person ORDER BY person",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn recomputed_visible(conn: &Connection) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare(
            "SELECT v.person, g.resource FROM \
             (SELECT person FROM body_a UNION SELECT person FROM body_b UNION SELECT person FROM body_c) v \
             JOIN grant_resource g ON g.person = v.person ORDER BY v.person, g.resource",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
