# Review: lab 20260920.2, batching at xSync (PR #18)

Verdict: the savepoint claim holds on SQLite 3.53.2; the mark policy's spill path is wrong in three inputs the lab's suite never sends.

Build read: `libsqlite3-sys 0.38.2` (root `Cargo.lock:571`, lab `Cargo.lock:160`), amalgamation `SQLITE_VERSION "3.53.2"` (`sqlite3.c:470`), `rusqlite 0.40.2` (`Cargo.lock:909`). Amalgamation path: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libsqlite3-sys-0.38.2/sqlite3/sqlite3.c`.

## Claims

| # | claim | verdict | where |
| --- | --- | --- | --- |
| 1 | SQLite opens a statement savepoint for any statement that fires a trigger | holds, with two guards | `sqlite3.c:100615` `p->usesStmtJournal && pOp->p2 && (db->autoCommit==0 \|\| db->nVdbeRead>1)`; call at `sqlite3.c:100626` |
| 1a | a trigger sets `usesStmtJournal` | holds | `sqlite3.c:90443` `isMultiWrite && mayAbort`; INSERT `sqlite3.c:139781` `pSelect \|\| pTrigger`; UPDATE `sqlite3.c:160195` `pTrigger \|\| hasFK`; DELETE `sqlite3.c:133188` `bComplex`; the trigger body's vtab write sets `mayAbort` at `sqlite3.c:160947` |
| 1b | autocommit single statement opens none | holds | `sqlite3.c:100617`; lab covers it, `batch_spec.rs:171` |
| 2 | `sqlite3VtabSavepoint` calls `xSavepoint` on every vtab in `aVTrans`, touched or not | holds | loop `sqlite3.c:162835`, no per-statement filter; guard is `iVersion>=2` at `sqlite3.c:162838` and `pVTab->iSavepoint>iSavepoint` at `sqlite3.c:162853` |
| 2a | user `SAVEPOINT` also reaches `xSavepoint` | holds, only for vtabs already in `aVTrans` | `sqlite3.c:100336`; a vtab joining later gets one `xSavepoint` for the innermost index only, `sqlite3.c:162801` |
| 3 | FTS5 flushes on savepoint | holds, a full flush | `fts5SavepointMethod` `sqlite3.c:263304` calls `sqlite3Fts5FlushToDisk` `sqlite3.c:263294` → `sqlite3Fts5StorageSync` `sqlite3.c:265525` → `fts5IndexFlush` `sqlite3.c:257402`. `xRelease` flushes too when releasing below the last begun index, `sqlite3.c:263326` |
| 3a | citation `fts5.c:22248` | unverifiable | no `fts5.c` in the build; `0_pending.rs:32` cites `fts5.c:263304`, which is the `sqlite3.c` line. HYPOTHESIS.md:78 and the code disagree on the file |
| 4 | every sqlite_ivm source write fires a trigger | holds for DML | `src/1_maintenance.rs:403` creates AFTER INSERT/UPDATE/DELETE per source; guard BEFORE triggers at `:373`. `src/2a_source_ddl.rs:92` drops them during source DDL, so DDL rebuild writes never reach the vtab |
| 5 | truncating an append-only Vec restores the pre-savepoint state | fails in 3 inputs | defects 1 to 3 below; passes for spill inside a savepoint, spill then statement abort, nested spill with inner release (probed, see "Probed and passing") |
| 6 | 13 cases green, three runs | holds | `cargo test` in the lab: 13 passed, 3 of 3 runs, 0.01s to 0.03s |
| 7 | `PENDING_BYTE_CAP` 1 MiB is test-driven | mechanism yes, value no | `batch_spec.rs:131` passes `cap=96` via `cap_for(4)`; no test reaches `1 << 20` (`0_pending.rs:16`) |
| 8 | `PENDING_MARK_CEILING` 32 is test-driven | holds | `batch_spec.rs:207` loops `0..=32`, asserts `rollback_to/spilled == 1`, which only follows `flush_through_marks` `0_pending.rs:138` |
| 9 | rusqlite leaves the savepoint trio null at `iVersion` 1 | holds | `rusqlite-0.40.2/src/vtab/mod.rs:114` sets `iVersion: 1` once; `update_module_with_tx` `:185` sets only `xBegin/xSync/xCommit/xRollback` |
| 10 | the engine's transmute patch is the same shape | holds | `src/2_vtab.rs:15` transmutes `Module::update_module()` and sets `iVersion = 3`, `xRename`, `xShadowName`; the lab's `1_vtab.rs:29` does the same on `update_module_with_tx()` with `iVersion = 2`. The port replaces `update_module()` with `update_module_with_tx()` and adds three fields |

## Defects

Each case is in `labs/20260920.2.the-gang-copies-the-fts5-homework/tests/review_spilled_flag.rs`. Run:

```
cd labs/20260920.2.the-gang-copies-the-fts5-homework
RUST_LOG=error cargo test --test review_spilled_flag
```

Result on `7551b9e`: 0 passed, 3 failed.

### 1. `spilled` outlives its transaction

`Pending::take` (`0_pending.rs:91`) clears `rows` and `marks` and leaves `spilled`. Only `discard` (`0_pending.rs:96`) resets it. A committed transaction that breached the mark ceiling poisons every later transaction: the first `ROLLBACK TO` whose savepoint predates the vtab joining `aVTrans` reaches `rewind`'s `None` arm with `spilled == true` and returns `Err` (`0_pending.rs:132`).

Input, `policy=mark`:

```
BEGIN; INSERT INTO base VALUES(0,0,0);
SAVEPOINT s0; INSERT ...; ... SAVEPOINT s32; INSERT ...;   -- 33 nested, ceiling breached
COMMIT;                                                     -- take() keeps spilled=true
BEGIN; SAVEPOINT a; INSERT INTO base VALUES(100,5,5); ROLLBACK TO a;   -- SQL logic error
```

Test: `the_spill_flag_outlives_the_transaction_that_set_it`.

### 2. Spurious error on a savepoint older than every row

A `SAVEPOINT` issued before the vtab's first write in the transaction never receives `xSavepoint` (`sqlite3.c:100336` iterates `aVTrans`, and the vtab is not in it yet). All rows the buffer ever held in this transaction are younger than that savepoint, so the pager unwinds every flushed row and clearing the buffer is exact. `rewind` returns `Err` anyway because `spilled` is set.

Input, `policy=mark`:

```
BEGIN; SAVEPOINT outer_;
SAVEPOINT s0; INSERT ...; ... SAVEPOINT s32; INSERT ...;   -- ceiling breached
ROLLBACK TO outer_;                                         -- SQL logic error, should succeed
```

Test: `a_rollback_to_a_savepoint_older_than_every_row_succeeds_after_a_ceiling_flush`.

### 3. The loud error does not protect the arrangement

`xRollbackTo` returning an error aborts the `ROLLBACK TO` statement only (`sqlite3.c:100466`). The pager has already unwound the flushed-through rows (`sqlite3.c:100426` runs before `:100466`). The buffer keeps the rows staged after the flush. The transaction stays open. `COMMIT` succeeds, `xSync` writes the stale buffer, and the arrangement leaves the oracle.

Input, `policy=mark`:

```
BEGIN; INSERT INTO base VALUES(0,0,0);
SAVEPOINT s0; INSERT ...; ... SAVEPOINT s32; INSERT ...;
ROLLBACK TO s0;    -- SQL logic error, as the lab asserts
COMMIT;            -- Ok
```

| after COMMIT | value |
| --- | --- |
| arrangement | `[(0, 2, 2)]` |
| oracle | `[(0, 1, 0)]` |

The lab's `the_mark_ceiling_makes_a_deeper_rollback_fail_loudly` (`batch_spec.rs:202`) ends on `ROLLBACK`, so it never sees this. HYPOTHESIS.md:100 claims "returns an error rather than losing rows silently"; rows are lost on the next `COMMIT`. `xSync` has no check of `spilled`.

Test: `commit_after_the_loud_rollback_to_error_leaves_the_oracle`.

### 4. Re-entrant `&mut Table` during a cap spill (no test, undefined behavior)

The spill's upsert (`1_vtab.rs:151`) is `INSERT ... SELECT` with NOT NULL targets, so it has its own statement journal (`sqlite3.c:90443`) and, being nested (`nVdbeRead>1`, `sqlite3.c:100617`), opens a statement savepoint that iterates `aVTrans` and calls `xSavepoint` on this same table. `dispatch` (`1_vtab.rs:294`) forms `&mut Table` while `insert(&mut self)` at `1_vtab.rs:237` is live on the stack.

Measured, `policy=mark,cap=96`, one 10-row bulk insert inside `SAVEPOINT a` after one 2-row insert:

| span | count |
| --- | --- |
| `flush/cap` | 2 |
| `savepoint` | 5 |
| `release` | 4 |

Three `savepoint` entries belong to the three statements; two are re-entrant, one per spill. The engine port inherits this shape wherever `xUpdate` runs SQL against a connection whose `aVTrans` contains the table.

### 5. `release` keeps the mark it releases (no test, benign)

`Pending::release` (`0_pending.rs:111`) retains `at <= index`; SQLite's contract invalidates savepoints `>= index`. The stale mark raises `floor()` (`0_pending.rs:75`) for the next spill and counts against the ceiling until the next `mark` at the same index replaces it (`0_pending.rs:105`). Stack discipline in SQLite's numbering (`sqlite3.c:100337`, `:100626`) means the stale mark is always replaced before it can be matched by `rewind`, so no case changes the answer.

## Probed and passing

Cases written during review, run, and removed because they pass on `7551b9e`:

| input (`policy=mark,cap=96`) | result |
| --- | --- |
| 2 rows, `SAVEPOINT a`, 10-row bulk insert (2 spills), `ROLLBACK TO a`, `COMMIT` | arrangement == oracle |
| 2 rows, 7-row bulk insert whose 7th row violates the PK after 1 spill, `COMMIT` | arrangement == oracle |
| `SAVEPOINT a`, 1 row, `SAVEPOINT b`, 10-row bulk (2 spills), `RELEASE b`, `ROLLBACK TO a`, 1 row, `COMMIT` | arrangement == oracle |

`Pending::floor` (`0_pending.rs:75`) confines a spill to rows above the innermost mark, and the pager unwinds their pages on `ROLLBACK TO` or statement abort, so item 5's happy paths hold.

## Port notes

- The three defects share one root: `spilled` is a transaction-scoped flag stored on the table and reset only by full rollback. Reset it in `take`, and refuse `xSync` while it is set after a failed `rewind`.
- Defect 2 needs `rewind` to distinguish "no mark because the savepoint predates the join" from "no mark because `flush_through_marks` cleared it". Recording the join-time savepoint index at `xBegin` (`sqlite3.c:162801` passes it) separates the two.
- HYPOTHESIS.md:78 and `0_pending.rs:32` should cite one file for the FTS5 lines.
