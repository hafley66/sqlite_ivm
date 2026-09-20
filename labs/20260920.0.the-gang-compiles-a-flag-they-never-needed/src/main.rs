//! The smallest program that attaches a session and drains a changeset.

use lab_20260920_0::drain;
use rusqlite::session::Session;
use rusqlite::{Connection, Result};

fn main() -> Result<()> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("CREATE TABLE t(k INTEGER PRIMARY KEY, v INTEGER)")?;
    let mut session = Session::new(&db)?;
    session.attach::<&str>(None)?;
    db.execute("INSERT INTO t VALUES (1, 1)", [])?;
    let entries = drain(&session.changeset()?)?;
    for entry in &entries {
        println!(
            "{} {:?} indirect={}",
            entry.table, entry.action, entry.indirect
        );
    }
    println!("drained {} entries", entries.len());
    Ok(())
}
