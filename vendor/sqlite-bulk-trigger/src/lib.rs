//! One Rust callback per transaction for SQLite row triggers.
//!
//! @comment-ok: crate documentation, and the example below is a compiled doctest.
//!
//! SQLite fires triggers per row. A consumer that runs work inside that landing
//! pays for it once per row. This crate collects the rows into a virtual table,
//! the one SQLite object that receives `xSavepoint`, `xRollbackTo` and a
//! write-capable `xSync`, and hands the whole transaction to
//! [`BulkTrigger::on_batch`] once, at commit.
//!
//! ```
//! use rusqlite::Connection;
//! use sqlite_bulk_trigger::{watch, BulkTrigger, RowChange};
//!
//! struct Count(usize);
//! impl BulkTrigger for Count {
//!     fn on_batch(&mut self, _db: &Connection, batch: &[RowChange]) -> rusqlite::Result<()> {
//!         self.0 += batch.len();
//!         Ok(())
//!     }
//! }
//!
//! let db = Connection::open_in_memory()?;
//! db.execute_batch("CREATE TABLE orders(id INTEGER PRIMARY KEY, amount INTEGER)")?;
//! watch(&db, "orders_collector", &["orders"], Count(0))?;
//! db.execute_batch("INSERT INTO orders VALUES(1,10),(2,20)")?;
//! # Ok::<(), rusqlite::Error>(())
//! ```

mod collector;
mod schema;
mod vtab;

pub use collector::{BulkTrigger, Collector, Counts, RowChange, Sign, STAGED_BYTES, STAGED_ROWS};
pub use vtab::{counts, watch, Watch};
