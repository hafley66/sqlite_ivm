# The Gang Copies The Fts5 Homework

## Hypothesis

Buffering source rows in `xUpdate` and flushing in `xSync` makes maintenance
statements scale with transactions instead of rows. FTS5 already does this
(`fts5.c:21203`), so the shape is not speculative; what is unproven is whether
FTS5's savepoint policy survives contact with sqlite_ivm's trigger-driven write
path.

## Knob

The `TransactionVTab` seam at `rusqlite-0.40.2/src/vtab/mod.rs:356` plus the
three savepoint callbacks rusqlite leaves null, and the savepoint policy chosen
behind them.

## Invariants assumed of the input tables

- one source, integer group key and integer value, no NULLs, no joins; the
  aggregate is `COUNT(*)` and `SUM(v)` per group.
- source writes reach the virtual table only through AFTER INSERT / UPDATE /
  DELETE triggers, the positional hidden-column shape `src/1_maintenance.rs:388`
  generates.
- the oracle is a full recompute, `SELECT g,COUNT(*),SUM(v) FROM base GROUP BY g`,
  and every correctness case ends on it.

## Measurement

```
cd labs/20260920.2.the-gang-copies-the-fts5-homework
RUST_LOG=error cargo run -q     # the count table and the callback trace
RUST_LOG=error cargo test       # 13 cases
```

## Invalidates

Nothing else in the queue dies. Lab 1 ranked this lane at a 100 percent upper
bound because re-preparing the three maintenance statements per row costs about
75 percent of the fold; batching removes the per-row prepare entirely, so the
statement-cache half of `@statement-cache-thrash` stops being the first lever.

## Verdict

2026-09-20. `rusqlite 0.40.2`, `libsqlite3-sys 0.38.2`, SQLite 3.53.2.
Counts, not clocks: `cargo test` green three consecutive runs, 13 of 13, whole
battery under 0.02s.

### Does the shape hold

Yes. `xUpdate` runs no SQL, `xSync` flushes, `xCommit` does nothing failable,
and the arrangement lands on the oracle in all 13 cases.

The count assertion's two numbers, 100 inserts in one transaction:

| span | today (per-row maintenance) | this lab |
| --- | --- | --- |
| `stage/append` (Rust, no SQL) | n/a | 100 |
| `maintain/upsert` (the GROUP BY) | 100 | **1** |

`assert_growth` over a doubled input (50 to 100 rows) classes
`maintain/upsert` as `Growth::Constant` and `stage/append` as `Growth::Linear`.

### The finding the engine port has to watch for

**FTS5's savepoint policy collapses the batch to one flush per writing
statement.** Copying it verbatim gives back most of the win.

SQLite opens a statement-level savepoint for any statement that fires a trigger
(`sqlite3.c:100626`, guarded by `usesStmtJournal && db->autoCommit==0`), and
`sqlite3VtabSavepoint` (`sqlite3.c:162828`) then calls `xSavepoint` on every
virtual table in `aVTrans`. Every sqlite_ivm source write fires a trigger. FTS5
never sees this because a plain `INSERT INTO ft VALUES(...)` is a single vtab
write with no triggers, so it opens no statement journal.

Measured, 200 source rows in one transaction:

| policy | per-row statements | one bulk statement |
| --- | --- | --- |
| `Flush` (FTS5's, `fts5.c:22248`) | 200 maintenance statements | 1 |
| `Mark` | **1** | 1 |

`Policy::Flush` is the default and satisfies the acceptance list as written.
`Policy::Mark` records the buffer length at each savepoint instead of flushing,
and truncates back on `xRollbackTo`. It is safe here and not for FTS5: an
append-only `Vec<Staged>` can be truncated, and FTS5's destructively merged
pending hash cannot.

### What the mark policy costs

Two bounds, both named constants, both tested.

| bound | constant | what it protects | test |
| --- | --- | --- | --- |
| byte cap | `PENDING_BYTE_CAP`, 1 MiB, per-table `cap=` like FTS5's `hashsize` (`fts5.c:5465`) | resident buffer memory across a transaction | `the_byte_cap_flushes_early_and_the_answer_holds` |
| mark ceiling | `PENDING_MARK_CEILING`, 32 | one buffer segment per open savepoint, so memory stays under `(ceiling + 1) * cap` | `the_mark_ceiling_makes_a_deeper_rollback_fail_loudly` |

A cap-forced flush drains only rows above the innermost mark (`Pending::floor`).
Flushing below a mark would let a later `ROLLBACK TO` erase rows that belong to
the enclosing scope. Past the mark ceiling the buffer flushes through the marks
and sets a spill flag; a later rollback below them returns an error rather than
losing rows silently.

### FTS5's second flush trigger has no analog

`fts5.c:16326` flushes when `iRowid < iWriteRowid`, because its pending hash is
keyed on ascending rowid. Staged rows here carry a sign and the aggregate is
linear, so a batch is order-free: `SUM(sign)` and `SUM(sign*v)` give the same
answer under any permutation. `interleaved_inserts_deletes_and_updates_need_one_flush`
mixes four inserts, two deletes and one update in one transaction, takes one
flush, and matches the oracle.

The port must keep this property to keep the batch. Coalescing OLD and NEW into
one staged row breaks it by dropping a retraction, which is why
`one_update_stages_both_images` pins `stage/append == 2` for a single UPDATE.

### Other things the port has to watch for

- **rusqlite wires four of seven transaction callbacks.** `update_module_with_tx`
  (`vtab/mod.rs:185`) sets `xBegin`/`xSync`/`xCommit`/`xRollback` and leaves
  `xSavepoint`/`xRelease`/`xRollbackTo` null with `iVersion` at 1. The descriptor
  needs the same transmute patch `src/2_vtab.rs:15` already applies for `xRename`,
  plus `iVersion = 2`, or SQLite skips the savepoint callbacks entirely
  (`sqlite3.c:162838` tests `pMod->iVersion>=2`).
- **`CREATE VIRTUAL TABLE` joins `aVTrans`.** The DDL statement draws its own
  `xSync` and `xCommit` before any user write. Count assertions must exclude
  table construction or they read two extra callbacks.
- **One trigger-driven statement draws `xSavepoint` and `xRelease`, not just
  `xSavepoint`.** Under the flush policy the release point flushes mid-statement,
  which is why `rollback_discards_without_flushing` sees one flush under `Flush`
  and zero under `Mark`, with both leaving an empty arrangement.
- **Shadow-table writes from `xSync` are legal.** `sqlite3ReadOnlyShadowTables`
  (`sqlite3.c:129862`) exempts `sqlite3VtabInSync(db)`, so a defensive-mode
  connection still lets the flush write `<view>_state`.
- **`xCommit`'s return code is discarded by SQLite.** `commit()` only enters a
  `commit/pending_not_empty` span, which `x_commit_finds_nothing_left_to_do`
  asserts is never entered.
- **The engine already owns the staging table.** The flush writes rows into
  `<view>_pending` with one prepared insert, then runs one upsert and one prune.
  `<view>_delta` is the engine's existing equivalent, so the port reuses it and
  does not add a table.

### Instrument: build versus buy

| candidate | verdict |
| --- | --- |
| `hafley-observe 0.1.2` `CountRecorder` / `SpanCounts` / `assert_growth` | **taken.** Counts span entries by name and classes growth across a size ratio, which is the whole assertion. |
| lab 0's in-crate `1_observe.rs` | rejected. Lab 0 wrote it because the registry served `0.1.0` with no counters; `0.1.2` has them, so an in-lab copy would fork. |
| `hafley_observe::sqlite::instrument` | rejected. It answers query-plan questions, not statement counts, and costs a `warn!` per statement whose `fullscan_step > 0`. The `sqlite` and `otlp` features are off in this lab's manifest. |
| bespoke `Layer` counting `on_new_span` | rejected. It is `CountRecorder` with a different name. |
| lab 0's rig (`lab_20260920_0`) | not used. The rig generates wide tables for a wall-clock fold; this lab counts statements on one narrow table, so the rig's width axis adds build time and no evidence. |

### Untested, and why

- **Wall time.** The claim is a count. Durations are not deterministic and lab 1
  already priced the fold.
- **Concurrent writers.** SQLite is single-writer, so there is no case.
- **Joins, filters, multiple sources, the relational arrangement path.** The
  buffer is indifferent to the maintenance query behind it; substituting a
  richer query changes the flush SQL and not the batching.
- **The read path.** The cursor reads `<view>_state` and exists only so the
  oracle comparison can go through the virtual table.
- **The engine's own battery.** This lane never touches `src/**`, so it is
  unchanged by construction.
