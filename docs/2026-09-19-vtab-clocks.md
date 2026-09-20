# The virtual-table clocks

A virtual table is not one lifecycle. It is four, running at different rates,
and every performance question about `sqlite_ivm` is a question about which
clock a piece of work is nailed to.

| clock | fires on | cardinality per statement |
|---|---|---|
| schema | connection open, `CREATE`/`DROP VIRTUAL TABLE` | 0 or 1 |
| prepare | statement compile | 1 or more, never per row |
| row | each step of the VDBE | M, the row count |
| transaction | `BEGIN`, `COMMIT`, `ROLLBACK` | 1 |

Moving work down that table is the only lever. Work pinned to the row clock
multiplies by M. Work pinned to the transaction clock does not.

## Two x words that are not the same word

`xDisconnect` and `xDestroy` both tear down. They differ in what survives.

| method | frees the in-memory handle | drops the shadow tables |
|---|---|---|
| `xDisconnect` | yes | no |
| `xDestroy` | yes | yes |

Closing a connection calls `xDisconnect`. The data is still in the file, and
the next connection calls `xConnect` to reattach to it. Only `DROP TABLE`
reaches `xDestroy`.

The same split runs on the other end: `xCreate` mints the shadow tables and
runs once ever; `xConnect` reattaches to shadow tables that already exist and
runs once per connection. In this repo both funnel through one function,
`Table::attach`, with a boolean that says which (`src/2_vtab.rs:363-377`).
`destroy` calls `catalog::uninstall`; `disconnect` does not.

A vtab that is read-only for its whole life can declare `VTabKind::Eponymous`
and skip the pair entirely. This one cannot, because it owns storage.

## The read path

```sql
CREATE VIRTUAL TABLE v USING sqlite_ivm('SELECT a, count(*) FROM t GROUP BY a');
SELECT * FROM v WHERE k IN (4, 9, 11);
```

Line meanings on every board in this document:

| line | meaning |
|---|---|
| dashed, colored | SQL the caller wrote caused this |
| solid bold | immediate, exactly once |
| thick | high cardinality, multiplies by row count |
| finely dotted | eventual, deferred to a later clock |
| long dash | conditional, may never fire |

```d2
direction: right

sql: SQL the caller wrote {
  style.fill: "#1d3557"
  style.font-color: "#ffffff"
  ddl: CREATE VIRTUAL TABLE v USING sqlite_ivm(...)
  sel: SELECT * FROM v WHERE k IN (4,9,11)
}

schema: schema clock {
  style.fill: "#e8f0fe"
  create: xCreate  declare_vtab, mint shadow tables
  connect: xConnect  reattach, shadows already exist
  disconnect: xDisconnect  free the handle, data survives
  destroy: xDestroy  free the handle and drop the data
}

prepare: prepare clock {
  style.fill: "#fff4e0"
  best: xBestIndex  claims constraints, sets idxNum and argvIndex
}

read: row clock {
  style.fill: "#e9f7ef"
  open: xOpen  allocate one cursor
  filter: xFilter  receives argv for claimed constraints
  next: xNext  advance, xEof guards the loop
  column: xColumn  one call per column SQLite asks for
  close: xClose  free the cursor
}

sql.ddl -> schema.create: "immediate, exactly 1x ever" {style.stroke-dash: 3; style.stroke: "#1d3557"}
schema.create -> schema.connect: "1x per later connection to the same file" {style.stroke-dash: 5}
schema.connect -> schema.disconnect: "1x per connection close"
schema.disconnect -> schema.destroy: "only DROP TABLE reaches here" {style.stroke-dash: 5; style.stroke: "#c0392b"}

sql.sel -> prepare.best: "immediate, N>=1x per compile, never per row" {style.stroke-dash: 3; style.stroke: "#b8860b"}
prepare.best -> read.open: "plan handed down" {style.bold: true}
read.open -> read.filter: "1x per cursor restart"
read.filter -> read.next: "M rows, sequential" {style.stroke-width: 6}
read.next -> read.column: "M x requested columns" {style.stroke-width: 6}
read.column -> read.next: "resume until xEof is true"
read.next -> read.close: "at exhaustion"
```

Source: `docs/diagrams/vtab-read.d2`.

### State, per cursor

`xFilter` is a reset, not a start. SQLite may call it again on a live cursor
when a join re-drives the inner loop with a new outer row. A cursor therefore
holds two kinds of state, and conflating them is a bug:

| state | lifetime | reset by |
|---|---|---|
| the prepared statement, the buffers | `xOpen` to `xClose` | nothing |
| the current row, the bound args | one `xFilter` to the next | `xFilter` |

In this repo the cursor is `src/2_vtab.rs:455-485` and the reset lives in the
`filter` implementation.

### The IN-list cardinality, and `sqlite3_vtab_in`

Default behavior unrolls an `IN` list for you: `xFilter` runs once per element.

| step | default | with `set_in_constraint` |
|---|---|---|
| 1 | `xFilter(argv=[4])`, scan, exhaust | `xFilter(argv=[list handle])` |
| 2 | `xFilter(argv=[9])`, scan, exhaust | iterate 4, 9, 11 inside |
| 3 | `xFilter(argv=[11])`, scan, exhaust | done |
| total filter cycles | 3 | 1 |

Opt in from `xBestIndex`. `rusqlite` 0.40.2 surfaces this as
`IndexInfo::is_in_constraint` and `set_in_constraint`
(`rusqlite-0.40.2/src/vtab/mod.rs:602-612`), with the list walked through
`InValues`, a `FallibleIterator` over `sqlite3_vtab_in_first`/`_next`
(same file, `:893-895`). Both sit behind the `modern_sqlite` feature, which
this crate does not enable today (`Cargo.toml:26`). Read path only.

## The write path

```sql
-- the caller writes this
INSERT INTO source_table VALUES (1, 2, 3);

-- the generated AFTER trigger turns it into this, per row
INSERT INTO v(__ivm_source, __ivm_adding, __ivm_row)
VALUES (0, 1, json_array(NEW.a, NEW.b, NEW.c));
```

```d2
direction: right

sql: SQL the caller wrote {
  style.fill: "#1d3557"
  style.font-color: "#ffffff"
  ins: INSERT INTO source_table VALUES (...)
  trg: "AFTER trigger: INSERT INTO v(__ivm_source,__ivm_adding,__ivm_row)"
  cmt: COMMIT
}

write: row clock {
  style.fill: "#fdeaea"
  update: xUpdate  1x per mutated row, no bulk variant exists
  buffer: pending buffer  bounded, flushes early when full
}

txn: transaction clock {
  style.fill: "#f3e8fd"
  begin: xBegin  never nests, 1x per transaction
  sync: xSync  the only failable flush point
  commit: xCommit  return code is discarded by SQLite
  rollback: xRollback  drop the buffer, no flush
}

sql.ins -> sql.trg: "fires FOR EACH ROW, N rows means N firings" {style.stroke-dash: 3; style.stroke: "#c0392b"; style.stroke-width: 6}
sql.trg -> write.update: "immediate, 1x per trigger firing" {style.stroke-dash: 3; style.stroke: "#c0392b"}
txn.begin -> write.update: "opens the window, then any number of updates" {style.stroke-dash: 5}
write.update -> write.buffer: "append only, no SQL, no disk" {style.bold: true}
write.buffer -> write.update: "back-pressure: flush early past the byte cap" {style.stroke-dash: 2}
sql.cmt -> txn.sync: "immediate" {style.stroke-dash: 3; style.stroke: "#7d3c98"}
write.buffer -> txn.sync: "eventual, buffer drains exactly 1x" {style.stroke-dash: 2; style.stroke: "#7d3c98"; style.stroke-width: 6}
txn.sync -> txn.commit: "1x, nothing failable may live here" {style.bold: true}
txn.begin -> txn.rollback: "the other exit, exclusive of commit" {style.stroke-dash: 5}
```

Source: `docs/diagrams/vtab-write.d2`. The pending buffer is the proposed
state, not the current state. Today `insert()` does the whole maintenance
query inline (`src/2_vtab.rs:386`).

### Transaction-clock contract

| rule | consequence |
|---|---|
| `xBegin` never nests | one `xBegin` pairs with exactly one `xCommit` or `xRollback` |
| `xSync` runs on every participating vtab before `xCommit` runs on any | a two-phase prepare; one failure rolls the whole transaction back |
| `xCommit` and `xRollback` return codes are discarded | failable work in `xCommit` fails silently |
| `xSavepoint`, `xRelease`, `xRollbackTo` only occur between them | a buffer must be savepoint-aware or refuse savepoints |

The third row is the design rule: the flush goes in `xSync`.

## The maintenance row, in full

`HIDDEN` is the mechanism that makes a write into a call. Start from the
declaration (`src/2_vtab.rs:167`):

```sql
CREATE TABLE x(
  a, cnt,                        -- visible result columns
  __ivm_source INTEGER HIDDEN,
  __ivm_adding INTEGER HIDDEN,
  __ivm_row    TEXT    HIDDEN
)
```

That statement is never executed. `sqlite3_declare_vtab` parses it only to
learn column names, types, and the `HIDDEN` flags, then throws the table away.

What `HIDDEN` changes, precisely:

| behavior | visible column | HIDDEN column |
|---|---|---|
| appears in `SELECT *` | yes | no |
| appears in `PRAGMA table_info` | yes | no |
| addressable by name in `WHERE` | yes | yes |
| addressable by name in an `INSERT` column list | yes | yes |
| position in the `xUpdate` argv | after the two rowid slots | after every visible column |
| position in `xFilter` argv | by constraint | by constraint |

So a hidden column is a named parameter with a column's syntax. The write
below carries no data for `a` or `cnt`; it carries three arguments.

```sql
INSERT INTO v(__ivm_source, __ivm_adding, __ivm_row) VALUES (0, 1, '[7,9]');
```

Trace it:

| step | site | value |
|---|---|---|
| 1 | SQLite | routes the INSERT to `xUpdate` |
| 2 | `rusqlite` | wraps argv as `Inserts` |
| 3 | `src/2_vtab.rs:388` | `args.len()` must be visible-count + 5 |
| 4 | `src/2_vtab.rs:389` | every slot up to visible-count + 2 must be `Null`, else "managed results are read-only" |
| 5 | `src/2_vtab.rs:391` | slot `count+2` is `__ivm_source`, the table ordinal |
| 6 | `src/2_vtab.rs:393` | slot `count+3` is `__ivm_adding`, 1 or 0 |
| 7 | `src/2_vtab.rs:394` | slot `count+4` is `__ivm_row`, the JSON payload |
| 8 | `src/1_maintenance.rs:100` | `maintain` runs, writing the protected shadows |

The two rowid slots at the front of every `xUpdate` argv are why the offsets
start at `count+2`. Step 4 is the whole access-control story: a caller who
writes real values into the visible columns is trying to write the view, and
gets refused.

The generated trigger that produces these is at `src/1_maintenance.rs:399`.

## What is nailed to which clock today

> **Stale as of 2026-09-20.** This section describes storage format 3, whose
> JSON payload was removed by PR #1. Current main declares one hidden column
> per source column (`src/2_vtab.rs:59`) and no `json_extract` runs on the
> maintenance path. The measured replacement is in
> `labs/20260920.1.the-gang-finds-out-where-the-time-went/HYPOTHESIS.md`:
> nine steps, of which `step/validity`, `step/overflow`, and `step/upsert`
> carry 91.2 percent between them. The section below is kept because the
> clock argument still holds; only the step list changed.

Per single source-table row change, at M used columns:

| step | site | clock | cost at M=20 |
|---|---|---|---|
| guard `SELECT ... FROM pragma_recursive_triggers` | `src/1_maintenance.rs:369` | row | 1 vtab query |
| `typeof(NEW.col)!='integer'` checks | `src/1_maintenance.rs:378` | row | 20 |
| `json_array(...)` builds TEXT | `src/1_maintenance.rs:399` | row | 1 alloc, 20 encodes |
| `json_valid` + `json_type` + `json_array_length` | `src/2_vtab.rs:425` | row | 3 |
| `json_each` integer scan | `src/2_vtab.rs:434` | row | 20 |
| `DELETE FROM <view>_delta` | `src/1_maintenance.rs:125` | row | 1 write |
| `json_extract(?1,'$[i]')` per column | `src/1_maintenance.rs:118` | row | 20 |
| contributions `GROUP BY` | `src/1_maintenance.rs:127` | row | 1 query |

Everything is on the row clock. An `UPDATE` pays it twice, once for `OLD` and
once for `NEW` (`src/1_maintenance.rs:388`).
