# What FTS5 pinned to which clock

Companion to `docs/2026-09-19-vtab-clocks.md`. Same four clocks. FTS5 is the
in-tree precedent for moving write work off the row clock.

Line numbers are into `sqlite3.c` from `libsqlite3-sys-0.38.2`, SQLite 3.53.2,
after slicing lines 241057-269022 into `fts5.c`.

## The choice

| work | clock FTS5 chose | site |
|---|---|---|
| tokenize and index a row | row | `fts5UpdateMethod:21027` |
| write the index entry | row, **into memory only** | `sqlite3Fts5IndexWrite:16509` calls `sqlite3Fts5HashWrite` |
| write to disk | transaction | `fts5SyncMethod:21203` calls `sqlite3Fts5FlushToDisk` |
| confirm | nothing | `fts5CommitMethod:21215` is a no-op |

The comment above `fts5CommitMethod` states the rule outright: the pending
hash "has already been flushed into the database by fts5SyncMethod()."

## The bound

An unbounded buffer would be a blocking defect. FTS5's is bounded two ways
(`fts5.c:16326-16331`):

| condition | meaning |
|---|---|
| `nPendingData > nHashSize` | byte cap hit, flush early |
| `iRowid < iWriteRowid` | rowid went backwards, ordering broken, flush |

`FTS5_DEFAULT_HASHSIZE` is `1024*1024` (`fts5.c:4548`), tunable per table by
the `hashsize` option (`fts5.c:5465`).

So the buffer flushes at the transaction boundary **or** at a megabyte,
whichever comes first. That is the shape lab 5 should copy.

## The savepoint answer

A buffer that spans a transaction must survive `SAVEPOINT`. FTS5 does not try
to be clever: `fts5SavepointMethod:22248` and `fts5ReleaseMethod:22265` both
flush to disk, and `fts5RollbackToMethod:22283` discards. Savepoints become
flush points. Correct before fast.

## Read side

`fts5BestIndexMethod:19700` packs a bitmask into `idxNum`
(`fts5.c:19644` documents the bits) so `xFilter` knows which strategy was
chosen without re-deriving it. `fts5FilterMethod:20536` does the work.

## Storage

Five shadow tables, the whole list being the body of `fts5ShadowName:22795`:
`_config`, `_content`, `_data`, `_docsize`, `_idx`. Everything above the
b-tree (segments, merges, tokenizers, ranking) is built by FTS5 on the same
primitives this repo gets.

Segment compaction, for scale: `fts5IndexMergeLevel:14294`,
`fts5IndexAutomerge:14551`, `fts5IndexCrisismerge:14572`.

## Read back into the labs

| lab | changes because of this |
|---|---|
| 5, batch at the transaction boundary | stops being speculative, becomes "do what `fts5SyncMethod` does" |
| 5, savepoint question | answered: flush on savepoint and release, discard on rollback-to |
| 5, buffer bound | answered: byte cap plus an ordering trigger, both named constants |
