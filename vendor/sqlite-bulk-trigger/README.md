# sqlite-bulk-trigger

One Rust callback per transaction for SQLite row triggers, with savepoint
rollback handled inside. A consumer implements one trait and never touches the
virtual-table ABI.

## Why a virtual table

SQLite fires triggers per row. Work placed in that landing runs once per row:
200 inserted rows cost 200 rounds. A virtual table is the only SQLite object
that receives `xSavepoint`, `xRollbackTo` and a write-capable `xSync`, so it can
buffer a whole transaction and flush once at commit.

`sqlite3_preupdate_hook` sees every row but has no savepoint callback and cannot
write at commit, so it cannot do this job.

## Surface

```rust
pub enum Sign { Insert, Delete }

pub struct RowChange {
    pub table: String,
    pub sign: Sign,
    pub values: Vec<rusqlite::types::Value>,
    pub sequence: u64,
}

pub trait BulkTrigger: 'static {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> rusqlite::Result<()>;
}

pub fn watch<T: BulkTrigger>(db: &Connection, name: &str, tables: &[&str], trigger: T) -> Result<()>;
```

`Watch` is the builder form, for lowering the two spill caps.

`watch` installs, in one connection:

| object | name |
| --- | --- |
| collector virtual table | `<name>` |
| shadow table for spilled rows | `<name>_delta` |
| AFTER INSERT / DELETE / UPDATE trigger per watched table | `<name>_<table>_<event>` |

An UPDATE arrives as `Delete` of the old image then `Insert` of the new one.
`on_batch` is not called for an empty batch.

## Two paths over one state machine

| path | who owns the virtual table and the triggers | entry |
| --- | --- | --- |
| standalone | this crate | `watch`, `Watch` |
| embedded | the host, sqlite_ivm today | `Collector` |

An embedded host forwards its own callbacks:

| host callback | `Collector` |
| --- | --- |
| xBegin | `begin()` |
| xUpdate | `update(&Connection, RowChange) -> Result<()>` |
| xSavepoint | `savepoint(i32)` |
| xRelease | `release(i32)` |
| xRollbackTo | `rollback_to(i32)` |
| xSync | `drain(&Connection) -> Result<Vec<RowChange>>` |
| xCommit | `commit()` |
| xRollback | `rollback()` |

`Collector::new(name, width)` sizes the shadow table to the widest watched
table; `create_shadow` and `drop_shadow` build and remove it. `update` stamps
`sequence` from its own counter, so `RowChange::new` leaves the field at 0.
`drain` merges the memory rows with the shadow table in sequence order and
empties the shadow table.

`vtab.rs` calls these same methods, so the standalone path adds the ABI and
nothing else.

## Dependency

```toml
rusqlite = { version = "=0.40.2", features = ["vtab"] }
```

No `bundled`. A host that builds a loadable extension links the SQLite it is
loaded into, and a bundled copy in a dependency would link a second SQLite into
the dylib. The crate's own tests carry `bundled` as a dev-dependency.

## Payload encoding: one column per source column

The trigger body binds each source column to its own hidden column:

```sql
INSERT INTO collector(__source,__sign,__value0,__value1)
VALUES('orders',1,NEW."id",NEW."amount");
```

The alternative was `json_array(NEW.*)` with the blob and real encoders at
`sqlite_ivm/src/1a_relational.rs:306-316`. Column binding wins on three counts:

- SQLite hands xUpdate the `sqlite3_value` itself, so integer `1`, real `1.0`,
  `NULL`, blob and text arrive with their original types and need no decode
  pair. The two `CASE` shapes exist only because JSON cannot carry blob or real.
- The declaration's value columns carry no declared type, so no column affinity
  rewrites a value on its way in. A JSON payload column would need the same care
  plus a `json_each` scan per row.
- Narrower tables bind fewer columns; the collector cuts the padding using the
  arity it recorded for that table at `watch` time, so a trailing NULL stays a
  value and not padding.

The shadow table uses the same shape with an explicit `width` column, so a
spilled row is self-describing.

## Spill

Rows live in memory until a cap is reached, then go to `<name>_delta`.

```rust
/// Rows held in memory before the collector spills to its shadow table.
/// Protects the process from one transaction that inserts a whole file.
const STAGED_ROWS: usize = 10_000;

/// Bytes of payload held in memory before spilling. Same protection, for wide rows.
const STAGED_BYTES: usize = 8 << 20;
```

Design law: a mark is a position, `staged.len()`, plus a `sequence` on disk.
There is no "spilled rows past a mark" case to detect. The on-disk rows are
inside SQLite's own transaction, and `ROLLBACK TO` removes them by the pager
before `xRollbackTo` runs (`sqlite3.c:100426` precedes `:100466`). The collector
therefore carries no spill flag, and the three defects the batching lab's flag
produced have nowhere to live:

| lab defect | review cite | why it cannot happen here |
| --- | --- | --- |
| the flag outlives its transaction | `2026-09-20-batching-lab.md` §1 | no flag; `xBegin` and `xRollback` reset every field |
| a savepoint older than every row errors falsely | §2 | a missing mark means the savepoint predates the first write, so the collector resets to empty and returns OK |
| an error from `xRollbackTo` aborts one statement, then COMMIT flushes stale rows | §3 | `xRollbackTo` has no failure mode to report; it truncates and restores |

`ROLLBACK TO` also restores the sequence counter to its value at the savepoint,
so a delivered batch carries contiguous numbers from 0. The disk rows that held
the reused numbers are gone with the pager, so no collision survives.

`<name>_delta` is an ordinary table, not registered through `xShadowName`.
Spill writes happen in `xUpdate`, outside `xSync`, and
`sqlite3ReadOnlyShadowTables` exempts only `sqlite3VtabInSync`, so registering
the name would block the spill under `SQLITE_DBCONFIG_DEFENSIVE`.

## Callback cardinality

What SQLite promises, with the cites the review verified on 3.53.2:

| callback | fires | `sqlite3.c` |
| --- | --- | --- |
| xBegin | once, at the first collector write in a transaction | 162801 |
| xSavepoint / xRelease | once per trigger-firing statement, plus once per user `SAVEPOINT` / `RELEASE` | 100615, 100626 |
| xUpdate | once per source row | |
| xRollbackTo | once per `ROLLBACK TO`; an error return aborts only that statement | 100466, 100426 |
| xSync | once at COMMIT before pager commit; writes are legal | 129862 |
| xCommit | once after pager commit; return code discarded | |
| xRollback | once on transaction rollback | |

Measured by `three_statements_over_seven_rows_draw_the_documented_callbacks`:

| callback | count |
| --- | --- |
| begin | 1 |
| savepoint | 3 |
| release | 3 |
| rollback_to | 0 |
| update | 7 |
| sync | 1 |
| commit | 1 |
| rollback | 0 |

`CREATE VIRTUAL TABLE` joins the transaction and draws its own xSync and
xCommit, so `watch` zeroes `Counts` after its own DDL.

## Lifetime and limits

- One collector per connection. State is keyed by `(connection handle, name)` in
  a thread-local, so it survives the xDisconnect and xConnect pair SQLite runs
  whenever a schema reset reloads the table.
- The collector name doubles as the module name and must be an ASCII identifier.
- `xUpdate` cannot tell a trigger body from a hand-written INSERT. A direct
  insert naming a watched table with sign 1 or -1 is read as a change.
- Writing a watched table from inside `on_batch` re-enters the collector during
  its own flush. The trigger is moved out of its cell for the duration, so the
  re-entry gets an error rather than undefined behavior.
- `xCommit` checks its counters in memory only and reports a leftover batch
  through `tracing::error!`. SQLite discards the return code, and running SQL
  mid-commit is not worth the reach.

## Untested

| area | reason |
| --- | --- |
| performance | belongs in a lab, not a suite with a 2s budget |
| multiple connections | one collector per connection is the documented lifetime |
| `on_batch` writing a watched table | the re-entry error is documented, not a supported mode |
| `STAGED_ROWS` and `STAGED_BYTES` at their shipped values | the spill mechanism is proven at caps of 1 and 2; the shipped numbers are a memory budget, not a behavior |
