use hafley_observe::SpanCounts;
use lab_20260920_2::fixture;
use rusqlite::{Connection, Result};

type Shape = (&'static str, fn(&Connection, usize) -> Result<()>);

const SHAPES: [Shape; 2] = [
    ("per-row statements", fixture::seed_per_row),
    ("one bulk statement", fixture::seed_bulk),
];

const CALLBACKS: [&str; 7] = [
    "begin",
    "savepoint",
    "release",
    "rollback_to/discard",
    "sync",
    "commit",
    "rollback/discard",
];

fn measure(policy: &str, body: impl FnOnce(&Connection) -> Result<()>) -> Result<SpanCounts> {
    let db = fixture::connect(&format!("policy={policy}"))?;
    let (result, counts) = fixture::counted(|| body(&db));
    result?;
    assert_eq!(fixture::arrangement(&db)?, fixture::oracle(&db)?);
    Ok(counts)
}

fn main() -> Result<()> {
    println!("policy  shape                rows  stage/append  maintain/upsert  flush");
    for policy in ["flush", "mark"] {
        for (shape, seed) in SHAPES {
            for rows in [10usize, 100, 200] {
                let counts = measure(policy, |db| seed(db, rows))?;
                println!(
                    "{policy:<7} {shape:<20} {rows:<5} {:<13} {:<16} {}",
                    counts.entries_of("stage/append"),
                    counts.entries_of("maintain/upsert"),
                    counts.entries_of("flush")
                );
            }
        }
    }
    println!();
    println!("callbacks SQLite draws for one trigger-driven INSERT inside BEGIN/COMMIT:");
    let counts = measure("mark", |db| {
        db.execute_batch("BEGIN;INSERT INTO base(k,g,v) VALUES(1,1,10);COMMIT;")
    })?;
    for name in CALLBACKS {
        println!("  {name:<22} {}", counts.entries_of(name));
    }
    Ok(())
}
