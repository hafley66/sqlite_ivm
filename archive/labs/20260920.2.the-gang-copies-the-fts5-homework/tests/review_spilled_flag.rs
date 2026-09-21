//! Review cases from docs/reviews/2026-09-20-batching-lab.md. Each one fails on
//! the lab as merged in #18. Not fixed here.
use lab_20260920_2::fixture::{arrangement, connect, counted, oracle};
use lab_20260920_2::pending::PENDING_MARK_CEILING;
use rusqlite::{Connection, Result};

/// Nested savepoints past PENDING_MARK_CEILING, each carrying one insert.
fn breach_the_ceiling(db: &Connection) -> Result<()> {
    for depth in 0..=PENDING_MARK_CEILING {
        db.execute_batch(&format!("SAVEPOINT s{depth}"))?;
        db.execute_batch(&format!("INSERT INTO base(k,g,v) VALUES({},0,1)", depth + 1))?;
    }
    Ok(())
}

// Pending::take (0_pending.rs:91) clears rows and marks but leaves `spilled`
// set, so the flag from a committed transaction poisons the next one: a plain
// ROLLBACK TO in a fresh transaction hits rewind's Err branch (0_pending.rs:132).
#[test]
fn the_spill_flag_outlives_the_transaction_that_set_it() {
    let db = connect("policy=mark").expect("fixture");
    db.execute_batch("BEGIN; INSERT INTO base(k,g,v) VALUES(0,0,0);").unwrap();
    breach_the_ceiling(&db).unwrap();
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());

    let (result, counts) = counted(|| {
        db.execute_batch(
            "BEGIN;
             SAVEPOINT a;
             INSERT INTO base(k,g,v) VALUES(100,5,5);
             ROLLBACK TO a;
             COMMIT;",
        )
    });
    assert_eq!(counts.entries_of("rollback_to/spilled"), 0, "no rows were flushed in this transaction");
    result.expect("rollback to a savepoint in a fresh transaction");
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
}

// A savepoint opened before the table joins aVTrans never receives xSavepoint
// (sqlite3.c:100336 iterates aVTrans only), so it has no mark. Every buffered
// and every flushed row is younger than it, and the pager unwinds all of them.
// rewind (0_pending.rs:128) still returns Err because `spilled` is set.
#[test]
fn a_rollback_to_a_savepoint_older_than_every_row_succeeds_after_a_ceiling_flush() {
    let db = connect("policy=mark").expect("fixture");
    let (result, _) = counted(|| -> Result<()> {
        db.execute_batch("BEGIN; SAVEPOINT outer_;")?;
        breach_the_ceiling(&db)?;
        db.execute_batch("ROLLBACK TO outer_; COMMIT;")
    });
    result.expect("rollback to a savepoint older than every buffered row");
    assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
    assert!(arrangement(&db).unwrap().is_empty());
}

// The lab's ceiling test ends on ROLLBACK. The error from xRollbackTo leaves
// the transaction open (sqlite3.c:100466 aborts only the ROLLBACK TO
// statement), the pager has already unwound the flushed rows, and the buffer
// still holds rows staged after the flush. COMMIT then succeeds and xSync
// writes those rows: the arrangement carries phantom rows and has lost k=0.
#[test]
fn commit_after_the_loud_rollback_to_error_leaves_the_oracle() {
    let db = connect("policy=mark").expect("fixture");
    db.execute_batch("BEGIN; INSERT INTO base(k,g,v) VALUES(0,0,0);").unwrap();
    breach_the_ceiling(&db).unwrap();
    db.execute_batch("ROLLBACK TO s0").expect_err("rollback below a spilled mark must fail");
    let (commit, counts) = counted(|| db.execute_batch("COMMIT"));
    if commit.is_ok() {
        assert_eq!(counts.entries_of("flush"), 0, "COMMIT flushed a buffer the pager already unwound");
        assert_eq!(arrangement(&db).unwrap(), oracle(&db).unwrap());
    }
}
