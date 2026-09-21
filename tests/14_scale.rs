// Growth sweep: per-write cost of maintaining a view against recomputing its
// query, as the source grows. Prints a table under --nocapture; the assert is
// only that the view still equals the query at every cell.
#![cfg(not(feature = "extension"))]
use rusqlite::{types::Value, Connection, Result};
use sqlite_ivm::extension::register;
use std::time::Instant;

const SIZES: [i64; 5] = [10, 100, 1_000, 10_000, 100_000];
/// Single-row writes timed per cell, after the view is populated.
const WRITES: i64 = 20;

const QUERIES: [(&str, &str); 5] = [
    ("chain", "SELECT a.k AS c0,c.v AS c1 FROM a JOIN b ON a.v=b.k JOIN c ON b.v=c.k"),
    ("group", "SELECT k,count(*) AS n,sum(v) AS s FROM a GROUP BY k"),
    ("distinct", "SELECT DISTINCT v FROM a"),
    ("topk", "SELECT k,v FROM a ORDER BY v DESC,k DESC LIMIT 10"),
    ("semijoin", "SELECT a.k,a.v FROM a WHERE EXISTS(SELECT 1 FROM b WHERE b.k=a.v)"),
];

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut s = db.prepare(sql)?;
    let n = s.column_count();
    let mut out = s
        .query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
        .collect::<Result<Vec<Vec<Value>>>>()?;
    out.sort_by_key(|r| format!("{r:?}"));
    Ok(out)
}

fn seeded(n: i64) -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    register(&db)?;
    db.execute_batch(
        "PRAGMA recursive_triggers=ON;PRAGMA trusted_schema=ON;\
         CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
         CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
         CREATE TABLE c(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL);\
         CREATE INDEX a_v ON a(v);CREATE INDEX b_k ON b(k);CREATE INDEX b_v ON b(v);CREATE INDEX c_k ON c(k)",
    )?;
    db.execute_batch("BEGIN")?;
    for table in ["a", "b", "c"] {
        let mut insert = db.prepare(&format!("INSERT INTO {table}(id,k,v) VALUES(?1,?2,?3)"))?;
        // Keys and values spread over n/10 groups, fanout about 10 per join key.
        for id in 0..n {
            insert.execute((id, (id * 7) % (n / 10 + 1), (id * 13) % (n / 10 + 1)))?;
        }
    }
    db.execute_batch("COMMIT")?;
    Ok(db)
}

/// ms per single-row write with the view installed, then ms per recompute.
fn cell(name: &str, query: &str, n: i64) -> Result<(f64, f64, f64)> {
    let db = seeded(n)?;
    let started = Instant::now();
    db.execute_batch(&format!("CREATE VIRTUAL TABLE v USING sqlite_ivm('{}')", query.replace('\'', "''")))?;
    let populate = started.elapsed().as_secs_f64() * 1e3;
    let mut write = db.prepare("INSERT INTO a(id,k,v) VALUES(?1,?2,?3)")?;
    let mut remove = db.prepare("DELETE FROM a WHERE id=?1")?;
    let started = Instant::now();
    for i in 0..WRITES {
        write.execute((n + i, i % (n / 10 + 1), (i * 3) % (n / 10 + 1)))?;
        remove.execute([i])?;
    }
    let maintained = started.elapsed().as_secs_f64() * 1e3 / (WRITES * 2) as f64;
    // A recompute at 100k rows costs seconds; one sample is the whole budget.
    let recomputes = if n >= 100_000 { 1 } else if n >= 10_000 { 3 } else { WRITES };
    let started = Instant::now();
    for _ in 0..recomputes {
        rows(&db, query)?;
    }
    let recompute = started.elapsed().as_secs_f64() * 1e3 / recomputes as f64;
    assert_eq!(rows(&db, "SELECT * FROM v")?, rows(&db, query)?, "{name} n={n}");
    Ok((populate, maintained, recompute))
}

#[test]
#[ignore = "scale sweep, run by name: cargo test --release --test 14_scale -- --ignored"]
fn per_write_cost_against_recompute() -> Result<()> {
    let sizes: Vec<i64> = match std::env::var("IVM_SCALE_MAX") {
        Ok(max) => SIZES.iter().copied().filter(|n| *n <= max.parse().unwrap_or(i64::MAX)).collect(),
        Err(_) => SIZES.to_vec(),
    };
    println!("{:<10}{:>8}{:>13}{:>13}{:>13}{:>8}", "query", "n", "populate ms", "write ms", "recompute ms", "ratio");
    for (name, query) in QUERIES {
        for n in &sizes {
            let (populate, maintained, recompute) = cell(name, query, *n)?;
            println!(
                "{:<10}{:>8}{:>13.2}{:>13.3}{:>13.3}{:>8.2}",
                name, n, populate, maintained, recompute, recompute / maintained
            );
        }
    }
    Ok(())
}
