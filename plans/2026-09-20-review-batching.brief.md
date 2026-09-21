# Lane review-batching

Review one merged lab. No engine edits, no lab edits.

Base: `origin/main` @ `7551b9e`. Branch `review/batching`.

## What you are reviewing

`labs/20260920.2.the-gang-copies-the-fts5-homework/`, merged as PR #18.

Its claim, in its own words:

> SQLite opens a statement savepoint for any statement that fires a trigger
> (`sqlite3.c:100626`), and `sqlite3VtabSavepoint` then calls `xSavepoint` on
> every vtab in `aVTrans`. Every sqlite_ivm source write fires a trigger, so
> FTS5's flush-on-savepoint policy (`fts5.c:22248`) collapses the batch to one
> flush per writing statement: 200 maintenance statements for 200 per-row
> inserts. Marking the buffer instead of flushing it holds at 1, and is safe
> here because an append-only Vec can be truncated where FTS5's merged hash
> cannot.

If that claim is right, the engine port is straightforward. If it is wrong, the
port ships a batch that silently never batches.

## Verify these five, each with a file and line

1. Does SQLite open a statement savepoint for any statement firing a trigger?
   Check `sqlite3.c:100626` in the amalgamation this repo builds against, not a
   different version. Report the version you read.
2. Does `sqlite3VtabSavepoint` call `xSavepoint` on every vtab in `aVTrans`,
   including vtabs the statement did not touch?
3. Does FTS5 flush on savepoint at `fts5.c:22248`, and is that a flush or
   something weaker?
4. Does every sqlite_ivm source write fire a trigger? Read
   `src/2a_source_ddl.rs` and `src/2_vtab.rs`.
5. Is marking actually safe? The argument is that the buffer is an append-only
   `Vec` truncatable to a mark. Read
   `labs/20260920.2.the-gang-copies-the-fts5-homework/src/0_pending.rs` and say
   whether truncation restores the exact pre-savepoint state in every path,
   including a nested savepoint and a rollback-to that crosses a byte-cap flush.

Number 5 is the one that can be wrong in a way the lab's own tests miss. An
early flush at `PENDING_BYTE_CAP` writes rows before a later `ROLLBACK TO`
arrives. Work out what happens then and whether the lab covers it.

## Also check

- The lab reports 13 cases green over three runs. Run them yourself.
- `PENDING_BYTE_CAP` is 1 MiB and `PENDING_MARK_CEILING` is 32. Is each driven
  by a test that actually reaches it, or only named?
- The port blocker it names: `rusqlite`'s `update_module_with_tx` leaves
  `xSavepoint`/`xRelease`/`xRollbackTo` null at iVersion 1. Confirm against the
  `rusqlite` version in `Cargo.lock`, and confirm the transmute patch at
  `src/2_vtab.rs:15` is the same shape that would be needed.

## Output

One review document at `docs/reviews/2026-09-20-batching-lab.md`.

Structure: verdict first, one line, does the claim hold. Then a table, one row
per checked item: claim, verdict, `path:line`. Then any defect found, each with
the input that triggers it.

If you find a case the lab's tests miss, write the failing case as a test in the
lab's own suite and say it fails. Do not fix it.

## Files you own

```
docs/reviews/2026-09-20-batching-lab.md                          (new)
labs/20260920.2.the-gang-copies-the-fts5-homework/tests/**       (added failing cases only)
```

## Files you must not touch

```
src/**   tests/**   bench/**   scripts/**   Cargo.toml   plans/**   issues/**
labs/20260920.2.*/src/**        (read it, do not change it)
every other labs/ directory
```

## Style laws, inline

- No em dashes. No negative parallelism ("not X, Y"). No rhetorical closes.
- Banned: provenance, substrate, load-bearing, regime, "ground truth" (say oracle),
  "support" as a noun (say refCount).
- Every claim carries a `path:line` or a command that prints it.
- Tables for numbers. No stray numbers in sentences.
- Short sentences. Present tense. No "we".
- **If a sentence does not carry a number, a path, or an instruction, delete it.**

## Report back

`boop beep --no-wait --as review-batching sprefa-coordinator "<one line>"`

Commit before reporting done. One line: does the savepoint claim hold, and did
you find a case the lab misses.
