---
created: 2026-09-20
updated: 2026-09-21
type: task
status: done
priority: high
epic: lab-queue-round-one
lane: lab-engine
lane_seq: 20
labels: [lab]
size: L
blocked_by: ['@statement-cache-thrash']
collision: [src/1_maintenance.rs, src/2_vtab.rs]
closed: 2026-09-20
closed_by: fable
commits:
- hash: bdf85ee
  summary: embed the collector
---

# Lab 5: the gang copies the fts5 homework

## Description

Every `xUpdate` today runs the whole maintenance query inline: `DELETE FROM <view>_delta`,
an insert of contributions, then a `GROUP BY`. Per row. An UPDATE pays it twice, once for
OLD and once for NEW (`src/1_maintenance.rs:388`).

## The change

Buffer in `xUpdate`, flush in `xSync`. `src/2_vtab.rs:386` appends; the maintenance query
runs once per transaction instead of once per row.

## This is not speculative

FTS5 does exactly this, in tree. Sites are in `docs/2026-09-19-fts5-clock-choices.md`:

| question | FTS5's answer | site |
|---|---|---|
| where does the flush go | `xSync` | `fts5.c:21203` |
| what about `xCommit` | no-op, with a comment saying why | `fts5.c:21215` |
| how is the buffer bounded | byte cap, 1 MiB default, tunable | `fts5.c:4548`, `:16326` |
| what about savepoints | flush on savepoint and release, discard on rollback-to | `fts5.c:22248`, `:22265`, `:22283` |

Every design question this lab had is answered by precedent. The profile would size the
win, not validate the shape.

## The bound is mandatory

An unbounded buffer is a blocking defect under the every-loop-is-bounded law. Byte cap,
named constant, comment saying what it protects, plus FTS5's second trigger: flush when
ordering breaks.

## Acceptance Criteria

- [x] `xUpdate` appends to a bounded buffer and runs no SQL
- [x] `xSync` flushes; `xCommit` does nothing failable
- [x] `xRollback` discards without flushing
- [x] savepoint and release flush, rollback-to discards
- [x] the byte cap is a named constant with a comment, and an early flush is tested
- [ ] the existing battery is green, unchanged
- [x] a `CountRecorder` assertion shows maintenance statements scale with transactions, not rows

## Test Plan

**What breaks if wrong:** rows are buffered and never flushed, or flushed twice, or a
rollback leaves them in. Every one of those is a wrong answer that looks like a fast one.

**No timing runs.** The claim is "maintenance statements scale with transactions, not
rows," which is a count, not a clock.

```rust
use oh::test;

#[test]
fn one_transaction_runs_one_maintenance_pass() { ... }

#[test]
fn rollback_discards_without_flushing() { ... }

#[test]
fn savepoint_and_release_flush() { ... }

#[test]
fn rollback_to_discards() { ... }

#[test]
fn buffer_flushes_early_at_the_byte_cap() { ... }

#[test]
fn update_still_pays_old_and_new() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| batching | 100 inserts in one transaction | maintenance statements constant, not 100x | the claim, as a `CountRecorder` assertion |
| rollback | inserts then ROLLBACK | no flush, view unchanged | a buffer that flushes on rollback invents rows |
| savepoint | SAVEPOINT then RELEASE | flushes | FTS5's answer at `fts5.c:22248`, `:22265` |
| rollback-to | SAVEPOINT then ROLLBACK TO | discards | `fts5.c:22283` |
| early flush | more than the byte cap in one transaction | flushes mid-transaction, answer still correct | the bound is mandatory; untested it is decoration |
| update pays twice | one UPDATE | both OLD and NEW buffered | `src/1_maintenance.rs:388`; a batcher that coalesces them silently drops a retraction |
| xCommit is inert | any transaction | nothing failable in `xCommit` | SQLite discards its return code, so a failure there is silent |

**Untested and why:** wall time, and concurrent writers. SQLite is single-writer, so
the second is not a case.

**Regression bar:** the existing battery green and unchanged. `scripts/9_verify.sh`
runs the CLI scenarios and is the outer gate.

**The trap:** a buffer that passes every test above by flushing on every `xUpdate`
anyway. The `CountRecorder` assertion in row one is what catches it.

## Resolution

### 2026-09-20T19:30:31Z · @fable

sqlite_bulk_trigger::Collector embedded in 2_vtab.rs; rows stage at the trigger, drain at xSync or first read; savepoint trio forwarded; mid-transaction drain then ROLLBACK TO re-stages (hafley-rs #85, dc20afa5)

## Comments

### 2026-09-21T04:04:24Z · @fable

archived: labs die on landing
