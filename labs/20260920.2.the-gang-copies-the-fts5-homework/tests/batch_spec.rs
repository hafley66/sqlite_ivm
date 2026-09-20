use hafley_observe::{assert_growth, Growth, SpanCounts};
use lab_20260920_2::fixture::{
    arrangement, cap_for, connect, counted, oracle, seed_bulk, seed_per_row,
};
use lab_20260920_2::pending::PENDING_MARK_CEILING;
use rusqlite::{Connection, Result};

/// Counts only the body. The table is built outside the recorder because
/// CREATE VIRTUAL TABLE joins aVTrans and draws its own xSync and xCommit.
fn measure(policy: &str, body: impl FnOnce(&Connection) -> Result<()>) -> SpanCounts {
    let db = connect(&format!("policy={policy}")).expect("fixture");
    let (result, counts) = counted(|| body(&db));
    result.expect("body");
    assert_eq!(
        arrangement(&db).expect("arrangement"),
        oracle(&db).expect("oracle"),
        "{policy} arrangement left the oracle"
    );
    counts
}

// The claim, as a count. This assertion is the only thing that catches a buffer
// passing every correctness case by flushing on every xUpdate anyway.
#[test]
fn maintenance_scales_with_transactions_not_rows() {
    let counts = measure("mark", |db| seed_per_row(db, 100));
    assert_eq!(counts.entries_of("stage/append"), 100);
    assert_eq!(counts.entries_of("maintain/upsert"), 1);
    assert_eq!(counts.entries_of("flush"), 1);
}

#[test]
fn maintenance_is_constant_and_staging_is_linear_across_a_doubled_input() {
    let small = measure("mark", |db| seed_per_row(db, 50));
    let large = measure("mark", |db| seed_per_row(db, 100));
    assert_growth(&small, &large, "maintain/upsert", 2.0, Growth::Constant);
    assert_growth(&small, &large, "stage/append", 2.0, Growth::Linear);
}

// FTS5's savepoint policy costs a flush per writing statement: a source write
// fires a trigger, which opens a statement journal (sqlite3.c:100626).
#[test]
fn the_precedent_policy_flushes_once_per_writing_statement() {
    let per_row = measure("flush", |db| seed_per_row(db, 100));
    assert_eq!(per_row.entries_of("maintain/upsert"), 100);
    let bulk = measure("flush", |db| seed_bulk(db, 100));
    assert_eq!(bulk.entries_of("maintain/upsert"), 1);
}

#[test]
fn x_update_runs_no_sql_before_the_transaction_ends() {
    let db = connect("policy=mark").expect("fixture");
    let (staged, counts) = counted(|| -> Result<i64> {
        db.execute_batch("BEGIN")?;
        let mut insert = db.prepare("INSERT INTO base(k,g,v) VALUES(?1,0,?1)")?;
        for k in 0..20i64 {
            insert.execute([k])?;
        }
        drop(insert);
        let seen: i64 = db.query_row("SELECT COUNT(*) FROM totals_state", [], |r| r.get(0))?;
        db.execute_batch("COMMIT")?;
        Ok(seen)
    });
    assert_eq!(staged.expect("body"), 0, "arrangement written before COMMIT");
    assert_eq!(counts.entries_of("stage/append"), 20);
    assert_eq!(counts.entries_of("maintain/upsert"), 1);
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
}

// Under Policy::Flush the row is already on disk when ROLLBACK arrives, because
// the statement's own xRelease flushed it; SQLite unwinds those pages.
#[test]
fn rollback_discards_without_flushing() {
    for (policy, flushes) in [("flush", 1), ("mark", 0)] {
        let db = connect(&format!("policy={policy}")).expect("fixture");
        let (result, counts) = counted(|| -> Result<()> {
            db.execute_batch("BEGIN")?;
            db.execute_batch("INSERT INTO base(k,g,v) VALUES(1,1,10),(2,2,20)")?;
            db.execute_batch("ROLLBACK")
        });
        result.expect("body");
        assert_eq!(counts.entries_of("flush"), flushes, "{policy}");
        assert_eq!(counts.entries_of("rollback/discard"), 1, "{policy}");
        assert!(arrangement(&db).unwrap().is_empty(), "{policy}");
        assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap(), "{policy}");
    }
}

#[test]
fn savepoint_then_release_keeps_the_answer() {
    for policy in ["flush", "mark"] {
        let counts = measure(policy, |db| {
            db.execute_batch(
                "BEGIN;
                 INSERT INTO base(k,g,v) VALUES(1,1,10),(2,1,20),(3,2,30);
                 SAVEPOINT s;
                 INSERT INTO base(k,g,v) VALUES(4,2,40),(5,3,50);
                 RELEASE s;
                 COMMIT;",
            )
        });
        assert!(counts.entries_of("savepoint") >= 1, "{policy}");
        assert!(counts.entries_of("release") >= 1, "{policy}");
    }
}

#[test]
fn savepoint_then_rollback_to_discards_only_its_own_rows() {
    for policy in ["flush", "mark"] {
        let counts = measure(policy, |db| {
            db.execute_batch(
                "BEGIN;
                 INSERT INTO base(k,g,v) VALUES(1,1,10),(2,1,20),(3,2,30);
                 SAVEPOINT s;
                 INSERT INTO base(k,g,v) VALUES(4,2,40),(5,3,50);
                 ROLLBACK TO s;
                 COMMIT;",
            )?;
            let surviving: i64 = db.query_row("SELECT COUNT(*) FROM base", [], |r| r.get(0))?;
            assert_eq!(surviving, 3, "{policy} base rows after rollback to");
            Ok(())
        });
        assert_eq!(counts.entries_of("rollback_to/discard"), 1, "{policy}");
    }
}

// An untested cap is decoration: four rows of cap against ten rows in one
// statement must flush twice before xSync and still land on the oracle.
#[test]
fn the_byte_cap_flushes_early_and_the_answer_holds() {
    let db = connect(&format!("policy=flush,{}", cap_for(4))).expect("fixture");
    let (result, counts) = counted(|| seed_bulk(&db, 10));
    result.expect("body");
    assert_eq!(counts.entries_of("flush/cap"), 2);
    assert_eq!(counts.entries_of("maintain/upsert"), 3);
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
}

// Coalescing OLD and NEW into one staged row would silently drop the retraction,
// so one UPDATE must append exactly two rows (src/1_maintenance.rs:388).
#[test]
fn one_update_stages_both_images() {
    let db = connect("policy=mark").expect("fixture");
    seed_bulk(&db, 1).expect("seed");
    let (result, counts) = counted(|| db.execute_batch("UPDATE base SET g=7, v=99 WHERE k=0"));
    result.expect("body");
    assert_eq!(counts.entries_of("stage/append"), 2);
    assert_eq!(counts.entries_of("maintain/upsert"), 1);
    assert_eq!(arrangement(&db).unwrap(), vec![(7, 1, 99)]);
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
}

// SQLite discards xCommit's return code, so anything failable there fails
// silently. The diagnostic span proves xSync already drained the buffer.
#[test]
fn x_commit_finds_nothing_left_to_do() {
    for policy in ["flush", "mark"] {
        for rows in [1usize, 40] {
            let counts = measure(policy, |db| seed_per_row(db, rows));
            assert_eq!(
                counts.entries_of("commit/pending_not_empty"),
                0,
                "{policy} with {rows} rows"
            );
            assert_eq!(counts.entries_of("commit"), 1, "{policy} with {rows} rows");
        }
    }
}

#[test]
fn an_autocommit_statement_still_flushes() {
    let db = connect("policy=mark").expect("fixture");
    let (result, counts) = counted(|| db.execute_batch("INSERT INTO base(k,g,v) VALUES(1,1,10)"));
    result.expect("body");
    assert_eq!(counts.entries_of("sync"), 1);
    assert_eq!(counts.entries_of("flush"), 1);
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
}

// A sign-carrying batch is order-free for a linear aggregate, so interleaving
// additions and retractions in one transaction needs no ordering trigger.
#[test]
fn interleaved_inserts_deletes_and_updates_need_one_flush() {
    let counts = measure("mark", |db| {
        db.execute_batch(
            "BEGIN;
             INSERT INTO base(k,g,v) VALUES(1,1,10),(2,1,20),(3,2,30),(4,2,40);
             DELETE FROM base WHERE k=2;
             UPDATE base SET v=99 WHERE k=3;
             INSERT INTO base(k,g,v) VALUES(5,3,50);
             DELETE FROM base WHERE k=1;
             COMMIT;",
        )
    });
    assert_eq!(counts.entries_of("maintain/upsert"), 1);
    assert_eq!(counts.entries_of("stage/append"), 4 + 1 + 2 + 1 + 1);
}

// The mark ceiling bounds Policy::Mark's memory. Past it the buffer flushes
// through the marks, and a rollback below them must fail instead of losing rows.
#[test]
fn the_mark_ceiling_makes_a_deeper_rollback_fail_loudly() {
    let db = connect("policy=mark").expect("fixture");
    let (result, counts) = counted(|| -> Result<()> {
        db.execute_batch("BEGIN")?;
        db.execute_batch("INSERT INTO base(k,g,v) VALUES(0,0,0)")?;
        for depth in 0..=PENDING_MARK_CEILING {
            db.execute_batch(&format!("SAVEPOINT s{depth}"))?;
            db.execute_batch(&format!("INSERT INTO base(k,g,v) VALUES({},0,1)", depth + 1))?;
        }
        db.execute_batch("ROLLBACK TO s0")
            .expect_err("rollback below a spilled mark must fail");
        db.execute_batch("ROLLBACK")
    });
    result.expect("body");
    assert_eq!(counts.entries_of("rollback_to/spilled"), 1);
    assert_eq!(counts.entries_of("rollback/discard"), 1);
    assert!(arrangement(&db).unwrap().is_empty());
}
