# Modern SQLite (2026): what it is, what it isn't, and what the complaint about your extension gets right

Audience: knows TypeScript, React, Redux, RxJS, medium SQL. Learning Rust.
Trigger: an extension that uses JSON text as b-tree keys, can't batch inserts,
has only row-level triggers, and can't hand a trigger row to Rust without a
virtual table. Reaction: "SQLite is shite."

This document is amended in four passes. Each pass is additive. Earlier
sections are kept and deepened, not replaced.

---

## Pass 1: the shape of the engine

### 1.1 Storage layout: b-trees, records, pages

A SQLite database file is one file, cut into fixed-size pages (default 4096
bytes, set at creation by `PRAGMA page_size`). Every table and every index is
a **separate b-tree** inside that one file. Page 1 doubles as the root page of
`sqlite_schema`, the table that stores the whole schema as SQL text.
Source: [Database File Format](https://www.sqlite.org/fileformat.html).

Two b-tree flavors:

| B-tree kind | Key | Used for |
|---|---|---|
| table b-tree | 64-bit signed integer rowid | ordinary ("rowid") tables |
| index b-tree | the indexed column values, table key appended | every `CREATE INDEX`, and every `WITHOUT ROWID` table |

Interior pages hold keys plus child-page pointers; leaf pages hold the actual
cells (row payloads for a table b-tree, index-key payloads for an index
b-tree). This is a page-oriented B-tree, not an LSM tree: writes go in place
via copy-on-write of the touched pages, there's no background compaction.
Think of it as the on-disk equivalent of a plain JS `Map` sorted by key, one
per table and one per index, all sharing the same page pool.

**Record format** (the payload format for both table-btree data and
index-btree keys): a record is a header (varint header-length, then one
varint "serial type" per column) followed by the raw column bytes back to
back. A **varint** is SQLite's own variable-length integer encoding (1-9
bytes, not protobuf LEB128): small values cost 1 byte, values needing more
range cost more. Source:
[Database File Format §Record Format](https://www.sqlite.org/fileformat.html#record_format).

Serial types cover: NULL, signed integers of width 1/2/3/4/6/8 bytes
(big-endian two's complement, smallest width that fits), the *constants* 0
and 1 (stored in **zero** payload bytes; a boolean-ish column costs nothing
per row beyond the serial-type varint), IEEE-754 float64, BLOB, and TEXT
(length is carried in the serial type itself).

**Why a TEXT key costs more than an INTEGER key**, concretely:

- An `INTEGER PRIMARY KEY` column is *not stored at all* in the record; SQL
  NULL goes in the record slot, and the real value comes from the b-tree key
  (the rowid) itself, which the b-tree page format carries as a 1-9 byte
  varint. A small integer key can cost as little as 1 byte of key material,
  and it participates in a fixed-width binary comparison during b-tree
  search.
- A TEXT key must carry: a serial-type varint encoding its byte length, then
  the raw UTF-8 (or UTF-16, but that's rare) bytes themselves, in full, at
  every level of the index where that key appears. Comparison is a
  byte-by-byte `memcmp`-class collation, not a fixed-width integer compare.
  Long keys spill to overflow pages past a per-page threshold, which is
  itself a second read.
- JSON text as a key compounds this: it's TEXT-affinity, usually much longer
  than a natural surrogate key, and canonicalization is the caller's problem:
  two JSON encodings of the same logical value (key order, whitespace,
  number formatting) are different byte strings and therefore different
  b-tree keys. This is not an SQLite defect; it is what "a JSON string is a
  TEXT value" necessarily means. See `sql-relational-design` and
  `sqlite-costs` skills in this workspace for the measured cost multiple on
  this machine.

**`WITHOUT ROWID`** ([sqlite.org/withoutrowid.html](https://www.sqlite.org/withoutrowid.html)):
available since 3.8.2 (2013-12-06). A `WITHOUT ROWID` table drops the
separate rowid b-tree and stores the table itself as a single index b-tree
clustered on its declared `PRIMARY KEY`; the SQLite equivalent of a
clustered index. Requirements: a `PRIMARY KEY` is mandatory, every PK column
must be individually `NOT NULL`, and PK columns must be collectively unique.
Ordinary rowid tables actually *violate* the SQL standard by allowing NULL in
declared `PRIMARY KEY` columns (a historic bug never fixed for compatibility);
`WITHOUT ROWID` tables are the standard-conformant ones. Not
interchangeable with any other engine's syntax; this is SQLite-only.

**`STRICT` tables** ([sqlite.org/stricttables.html](https://www.sqlite.org/stricttables.html)):
landed in 3.37.0 (2021-11-27). Add the `STRICT` keyword after the closing
paren of `CREATE TABLE`. Effect: columns are restricted to `INT`, `INTEGER`,
`REAL`, `TEXT`, `BLOB`, or `ANY`; a value that cannot be losslessly coerced to
the declared type raises `SQLITE_CONSTRAINT_DATATYPE` instead of silently
storing the wrong storage class. `ANY` columns opt back out, per column, and
still accept anything. There is no `ALTER TABLE ... STRICT`; migrating an
existing table means create-copy-drop-rename, which fails if bad data is
already present. Older SQLite (pre-3.37.0) cannot open a database containing
a `STRICT` table at all (one dump/reload footgun aside, noted on the same
page); this is a **file-format-visible** flag, not just a parser nicety.

### 1.2 Type affinity and manifest typing

SQLite uses **manifest typing**: the datatype is a property of the *value*,
not the column. Any storage class (`NULL`, `INTEGER`, `REAL`, `TEXT`, `BLOB`)
can land in any column regardless of declared type, subject to affinity
coercion. This is the sqlite.org framing, explicitly: "SQLite allows any
value of any datatype to be stored in any column regardless of the column's
declared type." Source: [Datatypes In SQLite](https://www.sqlite.org/datatype3.html).

This is **not** "dynamically typed" in the JS sense, and the distinction
matters for a Redux/TS reader:

- In JS, a variable has no declared type and nothing coerces on assignment. A
  SQLite column has a declared type, which is looked at *once* to pick one of
  five **affinities**; `TEXT`, `NUMERIC`, `INTEGER`, `REAL`, `BLOB`; and
  that affinity then actively *tries to coerce every incoming value* toward
  its preferred storage class before it's ever compared to the "no coercion
  happened" case. It's closer to a TypeScript `any`-typed field with a
  runtime coercer attached at the boundary than to untyped JS.
- Affinity rules (checked in this exact order against the declared type
  string): contains "INT" → INTEGER; contains "CHAR"/"CLOB"/"TEXT" → TEXT;
  contains "BLOB" or nothing declared → BLOB; contains "REAL"/"FLOA"/"DOUB" →
  REAL; otherwise → NUMERIC. This produces real surprises: `FLOATING POINT`
  gets INTEGER affinity (matches "INT" inside "POINT"); `STRING` gets NUMERIC,
  not TEXT.
- `STRICT` tables (1.37.0+) are the actual opt-out of manifest typing per
  table: non-`ANY` columns become type-checked at insert/update time, raising
  `SQLITE_CONSTRAINT_DATATYPE` on failure, rather than best-effort coercing
  and storing the result.

### 1.3 Collations

Three built-ins only, all documented at
[Datatypes In SQLite §Collating Sequences](https://www.sqlite.org/datatype3.html):

| Collation | Behavior |
|---|---|
| `BINARY` (default) | `memcmp()`-style byte comparison, encoding-aware only insofar as bytes are bytes |
| `NOCASE` | Same as BINARY but via `sqlite3_strnicmp()`; folds **only the 26 ASCII letters**; not Unicode, no locale, no ß/İ/ligature handling |
| `RTRIM` | Same as BINARY but trailing spaces (only spaces, not all whitespace) are stripped from both operands first |

Collation resolution for `=`, `<`, `>`, `<=`, `>=`, `!=`, `IS`, `IS NOT`:
explicit `COLLATE` on either operand wins (left operand's explicit collation
wins over right's), else a column operand's declared collation wins (left
before right), else `BINARY`. `ORDER BY` follows the same "explicit COLLATE,
else column's collation, else BINARY" chain.

Custom collations are registered via `sqlite3_create_collation()` and can
implement anything that is a reflexive, transitive, antisymmetric total
order; this is the escape hatch for real Unicode case-folding, locale
collation, or natural-sort. SQLite ships none of that itself; ICU-based
collation exists only via the (optional, separately built) ICU extension.

### 1.4 Indexes

- **Covering index**: an index that carries every column a query needs, so
  the table b-tree is never touched. `EXPLAIN QUERY PLAN` reports `USING
  COVERING INDEX`. Roughly halves the binary-search work versus a bare index
  lookup followed by a table lookup, because there's only one b-tree walk
  instead of two. Source:
  [Query Optimizer Overview](https://www.sqlite.org/optoverview.html).
- **Partial index**: `CREATE INDEX ... WHERE <expr>`. Only matching rows get
  index entries; smaller index, cheaper writes for excluded rows. The WHERE
  expression may reference only columns of the indexed table, no subqueries,
  no non-deterministic functions, no bound parameters. Can carry `UNIQUE` to
  enforce uniqueness over a subset of rows (e.g., "unique among non-deleted
  rows"). Source: [Partial Indexes](https://sqlite.org/partialindex.html).
- **Expression index**: index on an expression, not just a bare column;
  requires SQLite 3.9.0 (2015-10-14)+. Sharp gotcha for a TS/SQL-medium
  reader: the planner does **no algebra**. It matches only when the
  WHERE/ORDER BY expression is written *syntactically identical* (modulo
  whitespace) to the indexed expression. Two provably-equivalent expressions
  that are spelled differently get no match. Source:
  [Indexes On Expressions](https://sqlite.org/expridx.html).
- **`ANALYZE` / `sqlite_stat1`**: `ANALYZE` populates `sqlite_stat1(tbl, idx,
  stat)`, one row per index (or per table, if `idx` is NULL), where `stat` is
  `<nRow> <avg_eq_1> ... <avg_eq_K>`; row count, then average matches per
  equality constraint on the first N indexed columns. `sqlite_stat4`
  (requires the `SQLITE_ENABLE_STAT4` compile-time option; supersedes the
  deprecated `sqlite_stat2`/`sqlite_stat3`) adds histogram samples so the
  planner can reason about skewed value distributions, not just averages.
  `PRAGMA analysis_limit=N` (100-1000 is the documented rule of thumb) trades
  precision for speed on large tables, but disables `sqlite_stat4` sampling
  entirely if set nonzero. `PRAGMA optimize`, recommended to run just before
  closing each connection, runs `ANALYZE` selectively on tables the
  connection actually queried. Source: [ANALYZE](https://sqlite.org/lang_analyze.html).
- Without `ANALYZE` ever having run, the planner is flying on structural
  guesses only (heuristics baked into the optimizer, not measured
  cardinality); a table you swear is indexed can still get a full `SCAN` if
  the planner's guess about selectivity is wrong.

### 1.5 The query planner

SQLite has used the **Next-Generation Query Planner (NGQP)** since 3.8.0
(2013-08-26); described as a full rewrite for speed and plan quality over
the legacy planner. It's cost-based: for multi-way joins with many indexes
and subqueries, it picks among what can be a combinatorial number of
candidate plans, trying to minimize estimated disk I/O and CPU. Source:
[The Next-Generation Query Planner](https://www.sqlite.org/queryplanner-ng.html).

Reading the plan:

- `EXPLAIN QUERY PLAN <stmt>` gives the high-level plan as a small tree
  (id/parent/description rows). Documented as **for humans**: "the format of
  the output is subject to change... applications should not depend on it."
  `EXPLAIN <stmt>` (no "QUERY PLAN") instead dumps the actual VDBE bytecode
  the statement compiles to; the real ground truth of what will execute,
  much more verbose, not meant to be pretty. Source:
  [EXPLAIN QUERY PLAN](https://www.sqlite.org/eqp.html),
  [EXPLAIN](https://sqlite.org/lang_explain.html).
- `SCAN <table>` = full traversal (table or index, e.g. `SCAN t1 USING
  COVERING INDEX i1`). `SEARCH <table> USING INDEX i2 (a=? AND b>?)` = an
  indexed lookup, with the actual constraint terms shown. `MULTI-INDEX OR` =
  the OR-optimization splitting an `OR` into a union of indexed searches.
- `USE TEMP B-TREE FOR ORDER BY` / `... FOR GROUP BY` / `... FOR DISTINCT` =
  SQLite couldn't satisfy the ordering from an index and built a scratch
  b-tree to sort; almost always a "you're missing an index" signal. A
  `... FOR RIGHT PART OF ORDER BY` variant means only the tail of a
  multi-column `ORDER BY` needed the temp sort. `UNION USING TEMP B-TREE`
  appears for compound-query dedup, unrelated to row ordering.
- Join order: NGQP chooses it via its cost model; `CROSS JOIN` is the
  documented manual override to force left-to-right join nesting when the
  planner's automatic choice is wrong for a specific query (this is a real
  SQLite-specific idiom, not standard SQL semantics; `CROSS JOIN` in SQLite
  additionally disables the planner's freedom to reorder that join).

### 1.6 Transactions and locking

| Mode | Concurrency model | `SQLITE_BUSY` triggers |
|---|---|---|
| Rollback journal (default since 3.0) | One writer excludes all readers during commit | Can't get initial SHARED lock; hot-journal rollback contention; COMMIT can't get EXCLUSIVE because reader locks are outstanding; large-transaction cache spill widens the exclusive window early |
| WAL (available since 3.7.0, 2010-07-21) | One writer, many concurrent readers, readers never block writer and vice versa | `BEGIN IMMEDIATE`/`EXCLUSIVE` blocked by another live writer; WAL recovery (0→1 connections) takes an exclusive WRITER lock briefly; COMMIT itself cannot return `SQLITE_BUSY` in WAL mode |

Source: [Write-Ahead Logging](https://www.sqlite.org/wal.html),
[File Locking And Concurrency](https://sqlite.org/lockingv3.html).

`SQLITE_BUSY` is not "deadlock," it's "some other connection currently holds
a lock this operation needs, and no busy-handler/timeout resolved it in
time." A `busy_timeout` installs a handler that sleeps with backoff and
retries until the timeout; it does not change locking semantics, only how
patiently the call waits before surfacing `SQLITE_BUSY`.

**WAL file format**: a `-wal` file alongside the main database, header plus a
sequence of "frames," each frame recording one revised page image. Database
header bytes 18-19 both equal to 2 flags WAL mode in the file itself. Source:
[WAL-mode File Format](https://www.sqlite.org/walformat.html).

**2026 WAL corruption note** (dated, because it's exactly the kind of thing
worth knowing about): a data race nicknamed the "WAL-reset bug," present
potentially all the way back to 3.7.0, was found and fixed 2026-03-03,
shipped in 3.51.3 (2026-03-13), with backports to 3.44.6 and 3.50.7. It needs
two or more connections in separate threads/processes writing/checkpointing
at the exact same instant on the same WAL file; sqlite.org's own telemetry
estimate puts the occurrence rate at or below SSD-failure/cosmic-ray rates.
Not an emergency, but a real, dated, versioned data-integrity bug, which is
useful context against "SQLite never has bugs." Source: [Write-Ahead
Logging §2026 update](https://www.sqlite.org/wal.html).

**WAL2**: exists only as an experimental branch in the SQLite source
repository (not merged to trunk, not on the documented roadmap as of the
last public forum discussion found). Uses two WAL files to avoid the
checkpoint-blocks-writer stall a single WAL file can hit. Treat as
experimental/unshipped; do not depend on it. Marked further under
"Unverified" below for exact current branch status.

**`BEGIN CONCURRENT`**: also a branch feature (not in mainline release
builds), requires WAL or WAL2 mode. Multiple `BEGIN CONCURRENT` writers can
proceed at once using optimistic page-level locking; actual locking is
deferred to `COMMIT` time, which is still serialized one-at-a-time (a mutex
around commit is the documented recommendation for high contention).
Conflicts surface as `SQLITE_BUSY_SNAPSHOT`. Documented gotcha: a brand-new
b-tree is one page, so two inserts into the *same empty table* will conflict
even with disjoint data, purely because both touch that one page; inserts
into two different empty tables never conflict this way. Source:
[Begin Concurrent](https://www.sqlite.org/src/doc/begin-concurrent/doc/begin_concurrent.md).
Related, further-out prototype: **hctree**, aiming at optimistic *row-level*
locking for dozens of concurrent writers, distinct from `BEGIN CONCURRENT`'s
page-level locking.

**Savepoints**: nestable transactions (`SAVEPOINT name` / `RELEASE name` /
`ROLLBACK TO name`) layered on top of the one real transaction SQLite ever
has open at a time. A `ROLLBACK TO` undoes to the savepoint without ending
the outer transaction; `RELEASE` merges a savepoint into its parent. This is
the mechanism ORMs use to fake "nested transactions," since SQLite (like most
engines) has only one real transaction per connection at a time.

---

## Pass 2: the extension surfaces

This is the pass that speaks directly to the trigger: "cannot batch inserts,
only row-level triggers, cannot hand a trigger row to Rust without a virtual
table."

### 2.1 Virtual tables: the real protocol

A virtual table is a `sqlite3_module` struct of C function pointers
(`xCreate`, `xConnect`, `xBestIndex`, `xDisconnect`, `xDestroy`, `xOpen`,
`xClose`, `xFilter`, `xNext`, `xEof`, `xColumn`, `xRowid`, `xUpdate`,
`xBegin`, `xSync`, `xCommit`, `xRollback`, `xFindFunction`, `xRename`,
`xSavepoint`, `xRelease`, `xRollbackTo`, `xShadowName`, `xIntegrity`).
Source: [The Virtual Table Mechanism Of SQLite](https://www.sqlite.org/vtab.html),
[Virtual Table Object](https://www.sqlite.org/c3ref/module.html). Think of it
as implementing a `IService`-shaped adapter (in this repo's own vocabulary):
SQLite's VDBE bytecode is the caller, your `xMethod`s are the callee, and the
protocol is fixed by SQLite, not negotiable per-table.

**`xBestIndex`** is the one method that actually matters for performance, and
it runs at *prepare* time, once or more per query compile, not per row:

- SQLite fills `sqlite3_index_info` with the WHERE-clause constraints
  (`aConstraint[]`, each an operator + column) and any `ORDER BY`/`GROUP BY`
  request, derived from the whole query including joins.
- `xBestIndex` looks at those constraints and reports, via `aConstraintUsage`,
  which constraints it can use (setting `argvIndex` so the constraint's RHS
  value gets passed into `xFilter` as `argv[argvIndex-1]`), an `estimatedCost`
  (rough disk-access estimate), and optionally an `idxNum`/`idxStr` pair that
  `xFilter` will receive back to know which strategy was chosen.
- Constraint usability rule: a constraint is usable only if one side is a
  vtab column and the other side is a value knowable *before* the scan starts
 ; column-vs-column constraints on the same table are never usable this way.
- The `sqlite3_index_info` data is ephemeral; if you need to remember
  anything past `xBestIndex` returning, you must copy it (commonly into
  `idxStr` with `needToFreeIdxStr=1`), because SQLite may deallocate the
  struct as soon as the call returns.
- `sqlite3_vtab_in()` lets a vtab opt into seeing an entire `IN (...)` list at
  once instead of getting `xFilter` called once per value; this is the
  closest built-in mechanism to "batch reads through a vtab," and it's
  opt-in, per constraint, at `xBestIndex` time.
- Forum-documented practical advice: do the real work in `xFilter`/`xNext`,
  not in `xBestIndex`; `xBestIndex` should be cheap and can run many times
  per single query compile.

Source: [Virtual Table Indexing Information](https://www.sqlite.org/c3ref/index_info.html),
[sqlite3_vtab_in](https://www.sqlite.org/c3ref/vtab_in.html).

**Security/behavior flags**, set via `sqlite3_vtab_config()` from `xCreate`/`xConnect`:

| Flag | Effect |
|---|---|
| `SQLITE_VTAB_INNOCUOUS` | Declares the vtab safe to use from triggers, views, CHECK constraints, generated columns; i.e. from schema an attacker might control. Only set this if the vtab truly can do no harm even under hostile control. |
| `SQLITE_VTAB_DIRECTONLY` | The opposite: forbids use from triggers/views entirely, usable only from top-level SQL. Documented as the *recommended default posture* for a vtab with side effects. |
| `SQLITE_VTAB_CONSTRAINT_SUPPORT` | Declares that `xUpdate` detects constraint violations *before* mutating state, so SQLite can honor `ON CONFLICT` modes other than ABORT. Default (unset) means any `SQLITE_CONSTRAINT` from `xUpdate` rolls back the whole statement as if `OR ABORT` had been specified, regardless of the actual conflict mode requested. |

Untagged ("normal") vtabs behave as innocuous when `PRAGMA trusted_schema=ON`
and as direct-only when it's off. Source:
[Virtual Table Configuration Options](https://sqlite.org/c3ref/c_vtab_constraint_support.html),
[trusted-schema doc](https://www3.sqlite.org/src/doc/latest/doc/trusted-schema.md).

**`xShadowName`**: shadow tables are the ordinary SQL tables a vtab uses for
its own backing storage (e.g. FTS5's `_data`, `_idx`, `_content` tables).
They are read/write by ordinary SQL by default; `xShadowName` combined with
`SQLITE_DBCONFIG_DEFENSIVE` is what makes them read-only, closing off a class
of exploit where corrupting a shadow table via plain SQL crashes the vtab
implementation reading it back. Off by default for backward compatibility
(`.dump`/reload writes straight into shadow tables).

**Conflict handling in `xUpdate`**: with `SQLITE_VTAB_CONSTRAINT_SUPPORT`
enabled, call `sqlite3_vtab_on_conflict()` inside `xUpdate` to learn the
actual ON CONFLICT mode (`ROLLBACK`/`IGNORE`/`FAIL`/`ABORT`/`REPLACE`) and
implement `REPLACE` semantics yourself; if you can't, returning
`SQLITE_CONSTRAINT` falls back to ABORT behavior.

**Transaction callbacks**; this is the part most relevant to "cannot batch
inserts": `xBegin` fires once per vtab per transaction (never nested; one
`xBegin` always pairs with exactly one following `xCommit` or `xRollback`,
with any number of `xUpdate`/other calls in between). `xSync` is the optional
two-phase-commit prepare step, called on every participating vtab before
`xCommit` is called on any of them; if any `xSync` fails, the whole
transaction rolls back. `xCommit`/`xRollback` return codes are *ignored* by
SQLite; real failure-capable work belongs in `xSync`, not `xCommit`.
`xSavepoint`/`xRelease`/`xRollbackTo` only ever occur between an `xBegin` and
its matching `xCommit`/`xRollback`. Source:
[vtab.html §Transactions](https://www.sqlite.org/vtab.html).

**This is the actual batching mechanism SQLite gives a virtual table**: not a
bulk-insert method (`xUpdate` is still called once per row mutated by the SQL
engine; there is no `xUpdateMany`), but `xBegin`/`xSync`/`xCommit` as
natural buffering boundaries. A vtab that wants "batch insert" semantics
accumulates rows across repeated `xUpdate` calls inside one transaction and
flushes the batch in `xSync` or `xCommit`. **SQLite's own row-at-a-time
`xUpdate` call is the actual constraint your extension is hitting**; it is
real and structural, not a bug, but it is one level higher than "no
batching exists at all": the batching point is the transaction boundary
callback, not a call SQLite makes for you.

### 2.2 Triggers

Confirmed directly from [CREATE TRIGGER](https://www.sqlite.org/lang_createtrigger.html):
SQLite supports **only `FOR EACH ROW` triggers**. `FOR EACH ROW` is written
as optional syntax precisely because there is no alternative; no
`FOR EACH STATEMENT` exists. An `UPDATE` matching 400 rows fires a row
trigger 400 times, with no single "the statement is done" callback and no
way to see the whole affected set as one value.

This has been asked for and not delivered: a 2021-vintage SQLite forum
thread ("Transaction level triggers?") has a participant explicitly saying
"I've long wished for statement-level triggers in SQLite, as opposed to FOR
EACH ROW ones" (citing Oracle/Postgres experience) reframed by another poster
as a request for statement-level triggers specifically. No accepted proposal
or roadmap commitment was found; this reads as a standing, acknowledged gap,
not a planned feature. **Confirms the user's complaint as structurally
correct and not fixed as of 2026-09.**

Interactions worth knowing:

- **`recursive_triggers`**: off unless `PRAGMA recursive_triggers=1` is set
  per-connection, or the library is compiled with
  `SQLITE_DEFAULT_RECURSIVE_TRIGGERS=1`. With it off, a trigger that fires
  another trigger on the same table via cascading action may simply not
  cascade further. With it on, an unconstrained self-referential trigger
  (e.g. `UPDATE` trigger on table X that updates all of X) will infinite-loop
 ; restrict with `UPDATE OF <col>`, a `WHEN` clause that goes false after
  the change, and `WHERE rowid = NEW.rowid`.
- **`REPLACE` conflict resolution**: when `REPLACE` deletes a row to satisfy
  a uniqueness constraint, the deleted row's `DELETE` trigger fires **only
  if** `recursive_triggers` is on. `sqlite3_update_hook` is never invoked for
  those REPLACE-deleted rows, and the change counter isn't incremented for
  them either; sqlite.org's own docs flag this cluster of exceptions as
  possibly changing in a future release, i.e. it is documented as
  provisional/historical baggage, not settled design.
- **UPSERT (`ON CONFLICT DO UPDATE`) plus `REPLACE` inside a trigger body**:
  a genuine footgun. The `DO UPDATE` arm of an upsert is always executed as
  `DO UPDATE OR ABORT`, and an `ON CONFLICT` clause on the *outer* statement
  wins over any `ON CONFLICT`/`OR REPLACE` written inside a trigger body
  fired by it; so `INSERT OR REPLACE` statements inside a trigger silently
  degrade to plain `INSERT` (thus a `UNIQUE constraint failed`) when the
  trigger was fired by an upsert's `DO UPDATE`.
- **`RETURNING`**: deliberately reports only rows the statement directly
  targets, never rows changed by a foreign-key cascade or a trigger side
  effect, even in the same table. A `RETURNING ... RECURSIVE` extension was
  floated on the forum to opt into reporting same-table cascade rows, not
  shipped as of the sources found here. PostgreSQL made the identical choice
  since 9.6, for the identical reason (cascades can land in an arbitrary set
  of other tables, so no partial fix is consistent).
- **Foreign key cascades** are themselves implemented as internal trigger
  programs; SQLite added FK enforcement by generalizing the mechanism users
  were already hand-rolling with triggers before cascades existed natively.
  This means a cascade delete is, mechanically, a row trigger firing per
  affected row, with everything above about triggers applying to it too.

### 2.3 The hooks: what each can see that a trigger cannot

| Hook | Fires | Sees | Cannot see / do |
|---|---|---|---|
| `sqlite3_update_hook` | After each INSERT/UPDATE/DELETE on a **rowid table** | Operation type, db name, table name, rowid | No column values at all; nothing for virtual tables or WITHOUT ROWID tables in some builds; single registration per connection |
| `sqlite3_preupdate_hook` (needs `SQLITE_ENABLE_PREUPDATE_HOOK` at compile time) | Before each INSERT/UPDATE/DELETE | Old *and* new column values via `sqlite3_preupdate_old`/`_new`, column count via `sqlite3_preupdate_count`, nesting depth via `sqlite3_preupdate_depth` (0 = direct, 1 = top-level trigger, 2+ = nested trigger) | Does not fire for virtual tables or system tables (`sqlite_sequence`, `sqlite_stat1`); values only readable from inside the callback itself |
| `sqlite3_commit_hook` / `sqlite3_rollback_hook` | Once per transaction commit/rollback | That a commit/rollback happened; commit hook can veto by returning non-zero (forces a rollback) | No column data, no per-row detail; callback **cannot touch the connection** (no SQL, no prepare/step) until the triggering `sqlite3_step()` returns; full non-reentrancy |
| `sqlite3_wal_hook` | After a WAL-mode commit, after the write lock is released | DB name written to, page count currently in the WAL | Less constrained than commit/rollback hooks; you may read/write/checkpoint from inside it; setting `sqlite3_wal_autocheckpoint()` overwrites your hook, and passing your own hook disables auto-checkpointing (you own WAL growth now) |
| Session extension (changesets/patchsets) | Recorded continuously by an attached `sqlite3_session` object, extracted on demand | Whole-session diff, batched: coalesces multiple updates to the same row into one, drops no-op insert+delete pairs, groups by primary key across a whole session | Not a live callback; it's pull, not push; a **patchset** additionally strips old non-PK values, so `sqlite3changeset_invert()` cannot invert it and `SQLITE_CHANGESET_DATA` conflicts can't be detected from one |

**Direct answer on batch change notification**: the session extension is the
only one of these that natively batches. It has been in the amalgamation
since 3.13.0 (2016-05-18) but ships **disabled by default** and must be
compiled in. It is pull-based (you extract a changeset/patchset when you
want it, e.g. at commit), not a per-row push callback; which is exactly the
shape a Redux-style "collect a batch of actions, dispatch once" reducer
wants, and exactly what a row-level trigger or `update_hook` cannot give you.
None of `update_hook`, `preupdate_hook`, `commit_hook`, or `wal_hook` batch
anything; they are all still fundamentally per-row or per-transaction-event.
Source: [Session Extension intro](https://sqlite.org/sessionintro.html),
[sqlite3changeset_patchset](https://www.sqlite.org/session/sqlite3session_patchset.html).

**On "hand a row from a trigger to Rust without a virtual table"**: there is
in fact a hookless-of-vtab path, but it requires a compile flag most prebuilt
sqlite3 libraries don't ship with (`SQLITE_ENABLE_PREUPDATE_HOOK`) and it is
a C callback, not something reachable from pure trigger SQL; a trigger body
itself can only run more SQL, never call out to a host language. Getting a
row into Rust from inside a `CREATE TRIGGER` body genuinely has no path other
than: (a) a virtual table the trigger writes into, (b) a user-defined
scalar/aggregate function the trigger body calls (which *can* be a thin FFI
shim into Rust, registered via `sqlite3_create_function`), or (c) a
host-side hook (`preupdate`/`update`) observing the same connection from
outside SQL entirely, decoupled from any specific trigger. Option (b) is
underused and is probably the missing piece for this repo's specific
complaint: a custom scalar function can call into Rust per row without the
overhead of a virtual table's full method set.

### 2.4 Observability

| API | Grain | What it exposes | Requires |
|---|---|---|---|
| `sqlite3_trace_v2` | per statement / per row event | `SQLITE_TRACE_STMT` (statement begins, including each trigger subprogram), `SQLITE_TRACE_PROFILE`, `SQLITE_TRACE_ROW`, `SQLITE_TRACE_CLOSE`, OR-ed mask; supersedes deprecated `sqlite3_trace`/`sqlite3_profile` | nothing extra; public API |
| `sqlite3_stmt_status` | per prepared statement | `FULLSCAN_STEP` (rows stepped in a full scan), `SORT`, `AUTOINDEX` (automatic index built), `VM_STEP`, `REPREPARE`, `RUN`, `FILTER_HIT`/`FILTER_MISS` (Bloom-filter join skip effectiveness), `MEMUSED` | nothing extra |
| `sqlite3_db_status` | per connection | `CACHE_USED`/`CACHE_USED_SHARED` (pager cache bytes), `SCHEMA_USED`, `STMT_USED`, `CACHE_HIT`/`CACHE_MISS` | nothing extra |
| `sqlite3_status`/`_status64` | whole process/library | `MEMORY_USED`, page-cache overflow to malloc, high-water allocation size, parser stack depth (needs `YYTRACKMAXSTACKDEPTH`) | disabled if built `SQLITE_DEFAULT_MEMSTATUS=0` |
| `.scanstats` (CLI) / `sqlite3_stmt_scanstatus_v2` | per loop in a query plan | `NLOOP` (times a loop ran), `NVISIT` (rows examined), `EST` (planner's row estimate) per plan element, `SQLITE_SCANSTAT_COMPLEX` flag widens this to every EQP element not just SCAN/SEARCH loops | build-time `SQLITE_ENABLE_STMT_SCANSTATUS`; a documented gotcha is that `sqlite3.h` doesn't record which flags built the linked library, so a wrapper can compile clean and fail at load with an undefined symbol |

Source: [SQL Trace Hook](https://sqlite.org/c3ref/trace_v2.html),
[Prepared Statement Status](https://sqlite.org/c3ref/stmt_status.html),
[Database Connection Status](https://sqlite.org/c3ref/db_status.html),
[Status Parameters](https://www.sqlite.org/c3ref/c_status_malloc_count.html),
[Prepared Statement Scan Status](https://sqlite.org/c3ref/stmt_scanstatus.html).

### 2.5 Loadable extensions

Entry-point resolution when `sqlite3_load_extension(db, file, zProc=NULL,
...)` is called with no explicit proc name (from
[Run-Time Loadable Extensions](https://sqlite.org/loadext.html)):

1. Try `sqlite3_extension_init`.
2. Else try `sqlite3_X_init`, where `X` is the lower-cased filename stem
   (everything after the last `/` and before the first `.`, with any leading
   `lib` stripped).

Consequence: an extension built as `libfoo.so` with entry point
`sqlite3_foo_init` loads with no proc name needed; renaming the file breaks
auto-resolution silently until you pass `zProc` explicitly. Mixed-case entry
point names never auto-resolve, because the derivation lower-cases the
filename stem; this is a real, silent footgun for anyone naming their init
function to match a CamelCase crate name.

`sqlite3_auto_extension(xEntryPoint)` registers a function to run against
**every future new connection**; the mechanism for "load this extension's
functions/vtabs/collations into every connection this process opens,"
independent of any single `sqlite3_load_extension()` call. Despite the
zero-arg prototype, SQLite actually invokes it as
`int xEntryPoint(sqlite3*, char**, const sqlite3_api_routines*)`.
`sqlite3_cancel_auto_extension`/`sqlite3_reset_auto_extension` unregister one
or all. An init routine that returns `SQLITE_OK_LOAD_PERMANENTLY` keeps the
extension resident in the process but does **not** by itself register it on
future connections; that still requires the routine to also call
`sqlite3_auto_extension()` on a sub-registration function. Two are easily
confused; both are needed for "stays loaded and applies everywhere."

## Pass 3: what is new and what the ecosystem built

Current release as of this writing: **3.53.4** (2026-07-24), a maintenance
patch of the 3.53 line; next feature release expected as 3.54.0. Source:
[sqlite.org](https://sqlite.org/) home page,
[Recent SQLite News](https://sqlite.org/news.html).

### 3.1 Release-by-release, 2023 through 2026

| Version | Date | Landed |
|---|---|---|
| 3.41.0 | 2023-02-21 | `sqlite3_stmt_scanstatus_v2()` interface added |
| 3.42.0 | 2023-05-16 | `SQLITE_DBCONFIG_STMT_SCANSTATUS`, `SQLITE_DBCONFIG_REVERSE_SCANORDER` db_config options |
| 3.44.6 / 3.50.7 | later | backport targets for the 2026 WAL-reset fix (see 1.6) |
| 3.45.0 | 2024-01-15 | **JSONB**: binary JSON format, all JSON functions rewritten onto it; new `SQLITE_RESULT_SUBTYPE` function property |
| 3.45.1 | 2024-01-30 | JSONB hardening: fixed exponential-runtime bug and a 1-byte OOB read on corrupt JSONB input |
| 3.45.3 | 2024-04-15 | fixed incorrect `old.*` values in an UPDATE trigger fired by an UPSERT; `sum()` returning NULL where `Infinity` is more correct |
| 3.46.0 | 2024 | query-planner perf work, notably faster handling of `VALUES` clauses with many terms |
| 3.47.0 | 2024-10-15 | `sqlite3_rsync` CLI utility (bandwidth-efficient live backup over SSH); TCL9 integration |
| 3.48.0 | 2024/2025 | build system: Autotools replaced by Autosetup for the canonical configure script |
| 3.49.0 | 2025-02-06 | fixed a `concat_ws()` heap overwrite (separator string over 2MB) introduced in 3.44.0; hardened `SQLITE_DBCONFIG_LOOKASIDE` |
| 3.49.2 | 2025-05-07 | fixed a bug in the `NOT NULL` optimization dating to 3.40.0 |
| 3.50.0 | 2025-05-29 | `sqlite3_setlk_timeout()` (separate timeout from `busy_timeout` for blocking locks); `unistr()`/`unistr_quote()` functions; relaxed `SQLITE_DBCONFIG_ENABLE_COMMENTS` for reading pre-existing schema |
| 3.50.1 | 2025-06-06 | JSONB write-path optimization: `jsonb_set()`/`jsonb_replace()` on a large object now try to touch only the changed bytes/page, cutting I/O; fixed a long-standing `jsonb_set()` bug this optimization exposed |
| 3.50.4 | 2025-07-30 | fixed two long-standing uninitialized-variable bugs in obscure paths |
| 3.51.0 | 2025-11-04 | `jsonb_each()`/`jsonb_tree()` (JSONB-typed variants); `carray` and `percentile` extensions built into the amalgamation (opt-in via `-DSQLITE_ENABLE_CARRAY`/`-DSQLITE_ENABLE_PERCENTILE`) |
| 3.51.2 | 2026-01-09 | last version confirmed still exposed to the WAL-reset bug per the dated writeup |
| 3.52.0 | withdrawn | pulled after ship because new features (stale expression-index handling) weren't 100% backward compatible |
| 3.53.0 | 2026-04-09 | re-release of 3.52.0's content with the expression-index rework fixed; **Query Result Formatter (QRF)** library and CLI integration; TCL `format` method; **`opfs-wl`** VFS (OPFS + Web Locks for fairer browser lock sharing); WAL-reset bug fixed here |
| 3.53.4 | 2026-07-24 | maintenance patch, largely AI-discovered bug fixes; noted as possibly the last 3.53 patch |

Source: per-release logs at `sqlite.org/releaselog/3_XX_Y.html` and
[Release History Of SQLite](https://www.sqlite.org/changes.html).

Earlier landmark, for calibration since the user may not know it predates
this window: **`RETURNING`** shipped in 3.35.0 (2021-03-12); already old
news by 2023, not a "recent" feature, but frequently assumed to be newer
than it is.

**JSONB in plain terms for a TS reader**: not a new column type, not a
schema feature. It's an internal binary encoding that `json()`,
`jsonb()`,`->`, `->>`, and friends can read/produce instead of re-parsing
JSON text on every call; closer to "we cached the parsed AST as bytes" than
to "JSON got a real type." 5-10% smaller than text JSON, under half the CPU
cycles to process, roughly 3x faster on large JSON strings per SQLite's own
`test/json` measurements. It shares a name with PostgreSQL's `jsonb` but is
a **different, incompatible on-disk format**; not portable between the two
engines. Explicitly documented as private-to-SQLite, not an interchange
format, even though it is a stable, versioned on-disk format because it can
end up inside a database file meant to last decades. Source:
[The SQLite JSONB Format](https://sqlite.org/jsonb.html).

Math functions (`sqrt`, `sin`, `log`, `pow`, `trunc`, etc.) and
`unixepoch()` both predate this window in first landing but are easy to miss
if you learned SQLite years ago: math functions are compiled in only with
`-DSQLITE_ENABLE_MATH_FUNCTIONS` (not default-on in every build), and
`unixepoch()` always returns an integer even given millisecond input.
Window functions have been core SQLite since 3.25.0 (2018); anything
window-shaped built after 2023 in the table above is a planner-perf or
JSONB change, not a new window-function primitive; no major new window
function has landed in this window per the sources checked here.

### 3.2 The forks: libSQL and Turso Database

Two genuinely different projects as of 2026, easy to conflate because both
trace back to the same company (formerly ChiselStrike, now Turso):

| | **libSQL** | **Turso Database** (formerly "Limbo") |
|---|---|---|
| Relationship to SQLite | A **fork**; same C codebase lineage, patched | A **from-scratch rewrite in Rust**, not a fork |
| Maturity, 2026-09 | Battle-tested, "the safer production choice" per the project's own current guidance | Beta; maintainers explicitly withhold a "ready to replace SQLite" claim, target bar is "SQLite-level reliability" |
| File format | Always able to read/write standard SQLite files; emits standard files when extended features are unused | Aims for SQLite file-format and C-API compatibility |
| Concurrency | Inherits SQLite's single-writer model | MVCC-based, targets genuine concurrent writers |
| Notable additions | Embedded replicas, a network server mode, `ALTER TABLE` extensions (column type/constraint changes), randomized ROWID, WebAssembly UDFs, vtab implementations can see the raw SQL string (not just parsed constraints), a pluggable virtual WAL interface, native vector search, triggers with WASM-UDF bodies, Change Data Capture as queryable tables | Async-first I/O, MVCC, native vector search (DiskANN-based ANN at Turso Cloud scale), experimental Postgres wire-protocol support (`tursodatabase/turso`, described as "the LLVM of databases") |
| Statement-level triggers | **Not found**; inherits SQLite's row-only trigger model; the WASM-UDF trigger body is a workaround for *what a trigger can call*, not for *how many times it fires* | **Not found**; release notes (v0.6.0-era) mention `FOR EACH ROW` as the only clause; triggers exited "experimental" status but stayed row-only. `INSTEAD OF` triggers are parsed but documented as **not yet executed** as of the sources checked |
| Batch change notification | **Change Data Capture**: per-connection configurable, changes land in regular queryable tables in a binary format with JSON helper functions; this is the one project in this document that ships a real alternative to "poll a trigger row at a time." Positioned explicitly as a replacement for trigger-based or polling-based notification | Not found as a shipped feature distinct from libSQL's CDC in the sources checked |
| Extension ABI | No evidence of a redesigned virtual-table ABI; the "ABI" language in libSQL's own docs refers to the **WASM UDF calling convention**, not `xMethod` | No evidence found of a new/different vtab ABI as of 2026-09 |

**Direct read for the user's complaint**: neither fork has added statement-level
triggers. libSQL's Change Data Capture is the closest either project comes to
"batch change notification" as a first-class, non-trigger mechanism; worth
evaluating directly if the extension's actual need is "observe a batch of
changes," not "run inside every row's trigger." Source:
[tursodatabase/libsql](https://github.com/tursodatabase/libsql),
[libsql_extensions.md](https://github.com/tursodatabase/libsql/blob/main/libsql-sqlite3/doc/libsql_extensions.md),
[tursodatabase/turso](https://github.com/tursodatabase/turso),
[Turso CDC blog post](https://turso.tech/blog/introducing-change-data-capture-in-turso-sqlite-rewrite),
[Turso v0.6.0 notes](https://turso.tech/blog/turso-0.6.0).

### 3.3 Incremental view maintenance: what exists, what SQLite lacks

**pg_ivm** (PostgreSQL extension, not core Postgres, first released
2022-04-28 by SRA OSS): maintains "Incrementally Maintainable Materialized
Views" (IMMVs) by applying only the delta to a materialized view instead of
recomputing it, using **immediate** maintenance; the view is updated
inside `AFTER` triggers on the base table, in the same transaction as the
write. Compatible with Postgres 13-17. Known costs: blocks writes on the
base table until the view update finishes (a real throughput cost under
load), extra storage for the materialized result, and it is an extension
you must install, not a built-in capability of any managed Postgres.
Source: [pg_ivm repo](https://github.com/sraoss/pg_ivm),
[PostgreSQL Wiki IVM page](https://wiki.postgresql.org/wiki/Incremental_View_Maintenance).

**SQLite's answer**: none, natively. No materialized views at all (SQLite
`VIEW`s are always recomputed on every read, never cached), no built-in IVM
extension analogous to pg_ivm shipped by the SQLite project. Anyone wanting
this in SQLite is rolling their own; via triggers writing into a shadow
result table, via a custom virtual table, or via an external process reading
the WAL/session-extension changesets and recomputing views out of band. This
repo's own `sqlite_ivm` project is exactly that kind of from-scratch layer;
nothing upstream in SQLite removes the need for it.

**DBSP vs. differential dataflow, and which one SQLite's shape favors**:

- **Differential dataflow**: general model, partially-ordered logical
  timestamps, built for distributed, possibly-recursive, possibly-async
  dataflow with out-of-order inputs. Solves problems SQLite does not have
  (multiple writers with independent clocks, cross-node consistency).
- **DBSP**: assumes a *totally ordered* stream of inputs and does not permit
  revising past values mid-stream; a strictly weaker, cheaper model. A
  four-operator core covers relational algebra, aggregation, and (bounded)
  recursion, and the paper's contribution is deriving the incremental
  version of a query **automatically** rather than requiring a hand-written
  incremental algorithm per query shape.
- SQLite is single-writer with a strictly ordered commit sequence per
  connection-visible history; exactly the case DBSP was built for and
  differential dataflow's extra machinery would be pure overhead for. This
  is a real, checkable structural fit, not a vibe.
- **OpenIVM** (DuckDB-based, not SQLite, but the closest existing precedent
  in an embedded engine) compiles DBSP-style view-maintenance deltas down to
  ordinary SQL statements executed against the materialized-view table,
  rather than embedding a separate dataflow runtime; i.e., it reuses the
  host engine's own executor as the IVM engine. This is architecturally the
  most relevant fact for a SQLite-hosted IVM design: the delta computation
  does not require a new runtime living inside SQLite, only a way to express
  and drive delta SQL from base-table changes.
- Nobody has shipped a general-purpose DBSP/differential-dataflow-based IVM
  **for SQLite itself** in the sources checked here. This is a gap, not a
  solved-elsewhere-but-you-missed-it situation.
- Known hard parts, confirmed across the literature surveyed: deletion is
  harder than insertion (must account for everything a deleted row
  transitively supported); outer joins have their own dedicated body of IVM
  literature and are a known complexity spike; nested/recursive queries
  push you toward DBSP's Z-set (signed-weight multiset) representation,
  which is straightforward to bolt onto SQLite tables as an extra integer
  weight column but is still a design decision someone has to make, not
  something SQLite gives you.

### 3.4 The extension ecosystem: what people had to work around

All three of FTS5, R-Tree, and sqlite-vec are **ordinary virtual tables**
using the same `xMethod` protocol described in Pass 2; there is no special,
richer extension API reserved for "official" extensions. That itself is a
data point: the protocol a third-party author gets is the same one your
extension gets.

- **FTS5**: stores its real data in **shadow tables**; ordinary SQL tables
  named `<vtab>_content`, `<vtab>_segdir`, `<vtab>_segments`, etc. (FTS3/4
  naming shown; FTS5 follows the same shadow-table pattern), recognized via
  `xShadowName` (name contains `_`, prefix before the last `_` matches a
  vtab name). A **contentless** table (`content=''`) stores only the
  inverted index and not a private copy of row text, trading "no column
  values other than rowid are retrievable" for a much smaller table; the
  same batching/storage trade-off any log-structured index has to make.
  `content_rowid`, `columnsize`, `detail`, and `UNINDEXED` columns are all
  knobs for trimming exactly what gets duplicated into the index. FTS5
  authors had to build the entire segment-merge, tokenizer, and ranking
  machinery on top of the same read/write vtab primitives everyone else
  gets; nothing in core SQLite gave full-text search a shortcut. Source:
  [SQLite FTS5 Extension](https://www.sqlite.org/fts5.html).
- **R-Tree**: also a virtual table over shadow tables, providing spatial
  indexing (bounding-box queries) that SQLite's b-tree cannot do natively:
  a b-tree is one-dimensional-ordered by nature, so multi-dimensional range
  queries have no native index type at all without a vtab like this one.
- **sqlite-vec** (Alex Garcia / `asg017`, Mozilla Builders-sponsored):
  vector similarity search, architecturally a direct sibling of FTS5:
  `CREATE VIRTUAL TABLE ... USING vec0(embedding float[N])`, insert with
  plain `INSERT`, query with `... WHERE embedding MATCH '[...]' ORDER BY
  distance LIMIT k`. Pure C, no dependencies, runs anywhere SQLite runs
  including WASM in-browser. Ships brute-force search by default (a real
  scaling wall: ~6.1GB for one million 1536-dim float embeddings, hence bit-
  vector quantization support) with newer ANN indexes (`rescore`, `ivf`
  experimental, DiskANN) landing to address that wall. As of 2026, a
  community fork exists specifically because the original author was
  unavailable for a period; a real governance/bus-factor risk worth
  knowing about before depending on any single-maintainer extension,
  official-feeling or not.
- **The `asg017` family more broadly** (sqlite-html, sqlite-url,
  sqlite-path, sqlite-lines, sqlite-loadable-rs): a consistent pattern of
  "one small vtab or scalar-function extension per external format/concern,"
  distributed via `sqlite-dist` to GitHub releases, pip, npm, RubyGems,
  crates.io, sqlpkg, and Datasette simultaneously. `sqlite-loadable-rs`
  specifically exists to make **Rust** the implementation language for a
  loadable extension without hand-writing the C ABI shim yourself; directly
  relevant if the plan is a Rust-side vtab for this repo's own extension.

## Pass 4: the verdict and the cheat sheet

### 4.1 Is SQLite shite: a direct answer

**Structurally correct, not fixable by configuration:**

- Row-only triggers. Confirmed on [CREATE TRIGGER](https://www.sqlite.org/lang_createtrigger.html):
  `FOR EACH ROW` is the only mode that has ever existed. Neither libSQL nor
  Turso Database has added statement-level triggers as of the sources
  checked in Pass 3. An UPDATE touching N rows fires N trigger invocations,
  full stop. If the extension genuinely needs "run once, see the whole
  batch," a trigger is the wrong tool regardless of how it's written; this
  part of "SQLite is shite" is simply true.
- `xUpdate` is called once per row from the SQL engine's side too. There is
  no bulk-row `xUpdate` variant. The real batching seam SQLite offers a
  virtual table is the `xBegin`/`xSync`/`xCommit` transaction boundary
  (accumulate across calls, flush once); real, but one level removed from
  what "batch insert" usually means to someone coming from a bulk-write API.
- A trigger body cannot call into a host language directly. It can only run
  more SQL. The only way to reach Rust from inside triggered behavior is a
  scalar/aggregate function registered via `sqlite3_create_function` (thin,
  per-value FFI, no vtab machinery needed) or a virtual table the trigger
  writes into. There is no third door.

**Structurally correct, but fixed since the last time SQLite was learned:**

- Manifest typing being "no types at all"; wrong since `STRICT` tables,
  3.37.0, 2021-11-27. If the extension's tables predate this or were never
  upgraded, that's a decision left on the table, not an engine limit.
- "No binary JSON, everything is re-parsed text"; wrong since JSONB,
  3.45.0, 2024-01-15, roughly 3x faster on large JSON per SQLite's own
  measurements.
- "Change notification is only per-row polling"; wrong since the session
  extension shipped in the amalgamation (3.13.0, 2016-05-18, opt-in compile
  flag), which does whole-session coalesced changesets/patchsets. This
  predates the user's discovery of the problem by a decade; it was
  available the whole time, just off by default and undiscovered.

**The user's own design choice, not the engine's fault:**

- **JSON text as a b-tree key.** SQLite gives TEXT-affinity keys as one
  option among several; nothing forces JSON-as-key. A surrogate `INTEGER
  PRIMARY KEY` with the JSON stored as an ordinary (even `STRICT`,
  `ANY`-typed) column, or a dictionary-encoded natural key, is the standard
  fix; this repo's own `sql-relational-design` skill states this law
  explicitly for a reason: it's a recurring self-inflicted cost, not a rare
  edge case.
- **Cannot batch inserts, discovered as a surprise.** If the batching
  attempt was "call `xUpdate` many times and expect SQLite to coalesce
  them," that's a misunderstanding of the protocol, not a missing feature:
  the coalescing point is `xSync`/`xCommit`, and it has to be built by the
  vtab author.
- Foreign keys silently not enforcing: `PRAGMA foreign_keys` has defaulted
  to **OFF** since 3.6.19, explicitly to avoid breaking legacy databases,
  and it is a per-connection setting, not a database property; a fresh
  connection that forgets to set it gets zero FK enforcement with zero
  warning. This is a footgun, but it is a documented, stable, opt-in one;
  treating it as an engine defect after not reading the one-line pragma is
  a design choice.

### 4.2 Cost table

Where measured numbers exist (this machine, Apple M2 Pro, `:memory:`,
`libsql 0.5.29`/`rusqlite` bundled, 2026-08-06/07, from this repo's own
`sqlite-costs` skill; labs at `~/projects/hafley-rs`, `sqlite_raw/REPORT.md`
et al.), they are given as measured. Everything else is cited to sqlite.org
mechanism, not a number.

| Operation | Cost | Source |
|---|---|---|
| Bare rowid append, no index | ~10M rows/s | measured, `sqlite-costs` skill |
| Rowid table + one UNIQUE index | ~1.34M rows/s | measured |
| 4-column `WITHOUT ROWID` PK, all INTEGER | 3.3M -> 2.9M rows/s over 10k -> 1M rows (decays as tree deepens) | measured |
| 4-column `WITHOUT ROWID` PK, TEXT | 1.9M -> 1.5M rows/s, **consistently 1.7-2.0x slower than the equivalent INTEGER key**, across scales | measured |
| Interned INTEGER key vs raw TEXT key, whole fixpoint workload | 1.69-1.94x faster, 9.0-9.4x smaller on disk | measured |
| `rowid` + `UNIQUE` index vs `WITHOUT ROWID`, same algorithm | rowid+UNIQUE is 5.4-7.6% slower and 2.4x fatter on disk (stores every key twice: table plus index) | measured |
| In-process statement dispatch (2,582 calls) | ~4ms total; batching/fusing statement calls recovers nothing | measured; only deleting actual work helps |
| Covering index vs index + table lookup | roughly 2x fewer binary searches (one b-tree walk instead of two) | [Query Optimizer Overview](https://www.sqlite.org/optoverview.html) |
| `OR IGNORE` vs `NOT EXISTS` prefilter, duplicate insert | `OR IGNORE` wins ~1.4x at every duplication rate tested, on identical storage | measured |
| `ORDER BY` on an insert's source `SELECT` ("sorted append" theory) | measured loser, not a win | measured |
| Pragmas (`journal_mode`, `synchronous`, `cache_size`) on `:memory:` | no-ops; only `page_size=16384` has a real effect (~100MB RSS delta at 10M rows) | measured |
| Same pragma tuning on a **file-backed** db (`journal_mode=OFF` + `synchronous=OFF` + big `cache_size`/`mmap_size`) | ~2%; file-backed and `:memory:` were otherwise indistinguishable on this fold | measured |
| WAL mode vs rollback journal, general | WAL documented as "significantly faster in most scenarios," concurrent readers/writer | [Write-Ahead Logging](https://www.sqlite.org/wal.html) (no single multiplier given by sqlite.org; workload-dependent) |
| `sqlite3_vtab_in` batch `IN (...)` handling, opted in | avoids one `xFilter` call per RHS value, replacing N calls with 1 pass | [sqlite3_vtab_in](https://www.sqlite.org/c3ref/vtab_in.html) mechanism, not independently measured here |

The single fact most likely to reframe a "just batch it" instinct: **on a
real engine fold, SQLite was 11.9% of total wall time; 88% was
application-side Rust logic re-scanning its own already-collected rows.**
Measure the seam's actual share with `DL_TRACE_SUMMARY=1`-style
per-relation tracing (or the Rust equivalent: a span around each SQL call)
before proposing any SQL-side fix; the bottleneck is very often not SQLite.
Source: `sqlite-costs` skill, `~/projects/hafley-rs` measurement.

### 4.3 Reach for this

| Goal | Right mechanism | Notes |
|---|---|---|
| Cheap surrogate key | `INTEGER PRIMARY KEY` (rowid alias) | Free; stored as the b-tree key itself, zero record bytes |
| Natural key that's long/textual | Dictionary table: intern the TEXT once, reference by surrogate INTEGER everywhere else | `sql-relational-design` skill law in this workspace |
| Enforce a shape on write | `STRICT` table (3.37.0+) | Per-column `ANY` opt-out where genuinely needed |
| Store parsed JSON without re-parsing every read | JSONB via `jsonb()`, or let 3.45+ JSON functions produce it | Not an interchange format; don't export it raw to another system |
| Case-insensitive text match beyond ASCII | Custom collation via `sqlite3_create_collation()`, or an external ICU-based collation build | Built-in `NOCASE` folds ASCII only |
| "Did my query use the index I think it did" | `EXPLAIN QUERY PLAN`, read for `SCAN` vs `SEARCH` vs `USE TEMP B-TREE` | Not guaranteed stable across releases; for humans, not for gating CI |
| "Why didn't it use the index" | Run `ANALYZE` first; check `sqlite_stat1`; check expression-index exact-syntax match | Planner without stats is guessing from heuristics only |
| Spatial / bounding-box query | R-Tree virtual table | No native multi-dimensional index otherwise |
| Full-text search | FTS5, contentless if you don't need retrievable column text back | Shadow tables hold the real data; `.dump tblname` misses them |
| Vector similarity search | sqlite-vec (`vec0` virtual table) | Brute-force by default; check current ANN index maturity before assuming it scales past ~1M vectors |
| Audit log / CDC with actual old+new values | `sqlite3_preupdate_hook` (compile-time flag) or the session extension | `update_hook` alone gives you rowid only, no column values |
| Batch a burst of changes into one notification | Session extension changeset/patchset, or a fork with native CDC (libSQL) | No built-in push-based batch hook exists; this is pull/extraction-based |
| Get a trigger's row into Rust | A `sqlite3_create_function` scalar/aggregate FFI shim called from the trigger body | Cheaper than standing up a virtual table just to receive rows |
| Nested "sub-transactions" | `SAVEPOINT`/`RELEASE`/`ROLLBACK TO` | SQLite only ever has one real transaction per connection; this is the standard fake |
| Multiple concurrent writers to one file | **SQLite cannot, structurally, in mainline.** `BEGIN CONCURRENT`/`hctree` are unmerged branches; Turso Database (Rust rewrite) targets this natively but is beta in 2026 | Use a real multi-writer engine (Postgres) or accept single-writer serialization |
| Materialized, incrementally-updated view | **SQLite has no native answer.** Roll your own via triggers + a result table (pg_ivm's own approach, portable in spirit), or evaluate this repo's `sqlite_ivm` | Nothing upstream removes the need for a custom IVM layer |
| Statement-level "ran once, N rows changed" semantics | **SQLite cannot.** No `FOR EACH STATEMENT` trigger exists in any surveyed fork | Restructure as: application code issues the statement, then reads `sqlite3_changes()`, then does the "once" work itself |

### 4.4 The traps: silent wrong-or-slow with no error

| Trap | What actually happens | Where documented |
|---|---|---|
| `PRAGMA foreign_keys` defaults to **OFF** | FK constraints silently do not enforce; no error, no warning, per-connection not per-database | [SQLite Foreign Key Support](https://sqlite.org/foreignkeys.html); OFF by default since 3.6.19 |
| Expression index matched by syntax, not algebra | A logically-identical WHERE clause spelled differently than the index definition silently gets a full scan instead | [Indexes On Expressions](https://sqlite.org/expridx.html) |
| Affinity coercion on declared-type substring matching | `FLOATING POINT` gets INTEGER affinity ("INT" inside "POINT"); `STRING` gets NUMERIC, not TEXT | [Datatypes In SQLite](https://www.sqlite.org/datatype3.html) |
| `REPLACE` conflict resolution silently skips notification | `update_hook` never fires for REPLACE-deleted rows; delete triggers only fire if `recursive_triggers` is on; change counter isn't incremented; sqlite.org itself flags this cluster as possibly changing later | [ON CONFLICT Clause](https://www.sqlite.org/lang_conflict.html) |
| Upsert `DO UPDATE` forces `OR ABORT` on nested trigger REPLACE | `INSERT OR REPLACE` inside a trigger fired by an upsert's `DO UPDATE` silently becomes plain `INSERT`, surfacing as a `UNIQUE constraint failed` far from the real cause | [ON CONFLICT Clause](https://www.sqlite.org/lang_conflict.html), SQLite forum |
| `RETURNING` never reports cascade rows | A cascading delete/update from FK or trigger action is invisible to `RETURNING`, even in the same table | SQLite forum, matches Postgres 9.6+ behavior |
| Rowid reuse without `AUTOINCREMENT` | Deleting the highest-rowid row means the next insert can silently reuse that rowid value; `AUTOINCREMENT` (better read as "MONOTONIC," per the docs' own framing) exists specifically to prevent this and is not the default | [SQLite Autoincrement](https://sqlite.org/autoinc.html) |
| `NOCASE` is ASCII-only | Non-ASCII case folding (accents, non-Latin scripts) silently does not fold; comparisons "just don't match" with no error | [Datatypes In SQLite](https://www.sqlite.org/datatype3.html) |
| WAL file grows unbounded if you install your own `wal_hook` | Setting a custom `sqlite3_wal_hook` **disables** SQLite's automatic checkpointing; nothing errors, the `-wal` file just keeps growing until you checkpoint yourself | [Write-Ahead Log Commit Hook](https://sqlite.org/c3ref/wal_hook.html) |
| Commit/rollback hook re-entrancy | Running any SQL at all (even a `SELECT`) from inside a `commit_hook`/`rollback_hook` callback is undefined-behavior territory; it's not enforced by an error, it's just documented as forbidden | [Commit And Rollback Notification Callbacks](https://sqlite.org/c3ref/commit_hook.html) |
| Preupdate hook fires during `VACUUM` | Acknowledged on the SQLite forum as "awry" (library-or-doc bug), but as of the sources checked it still does this; a preupdate-based audit log can pick up spurious VACUUM-internal rows | SQLite forum thread on `sqlite3_preupdate_hook` and vacuum |
| `sqlite3_stmt_scanstatus_v2` silently unavailable | Compiles clean against `sqlite3.h`, then fails at **load time** with an undefined symbol if the linked `libsqlite3` wasn't built with `SQLITE_ENABLE_STMT_SCANSTATUS`; the header carries no record of how the library was actually built | SQLite forum, cited in Pass 2 §2.4 |
| `STRICT` table opened by old SQLite via `.dump` first | A pre-3.37.0 SQLite CLI can silently read/write a STRICT table without enforcing types if `.dump` runs first, potentially corrupting it; `PRAGMA quick_check` on a newer version is what catches it after the fact | [STRICT Tables](https://www.sqlite.org/stricttables.html) |
| `BEGIN CONCURRENT` conflicts on genuinely disjoint data | Two inserts into the *same brand-new empty table* conflict even with completely different row values, because a new b-tree is one page and both writes touch it | [Begin Concurrent doc](https://www.sqlite.org/src/doc/begin-concurrent/doc/begin_concurrent.md) |

## Unverified

- Exact current merge/release status of the `wal2` branch as of 2026-09: not
  found on a dated sqlite.org release-notes page in this pass. Treated above
  as "experimental branch, not mainline."
- Exact current status of `BEGIN CONCURRENT` and `hctree` mainlining as of
  2026-09: last confirmed public statement found was a 2024 forum reply
  saying it is not on a written roadmap.
- Whether any window-function *primitive* (not planner/perf change) has
  landed in core SQLite specifically between 2023 and 2026-09; none was
  found in the searched release logs, but the release-log coverage in Pass 3
  is not exhaustive of every point release's minor additions.
- Turso Database's and libSQL's precise current (2026-09) status on
  statement-level triggers and vtab ABI redesign rests on release notes and
  docs pages current as of the searches run for this document; both are
  fast-moving projects and could have shipped changes after this was
  written.
- No independent, dated benchmark was found quantifying WAL-mode's speedup
  over rollback-journal mode as a single multiplier; sqlite.org's own
  language is qualitative ("significantly faster in most scenarios"), so no
  number is given in the cost table for that row beyond the qualitative
  claim.
