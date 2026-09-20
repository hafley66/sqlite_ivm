---
created: 2026-09-19
updated: 2026-09-19
type: epic
owner: hafley66
status: open
priority: normal
---

# Labs 3 through 9, blocked on the attribution profile

## Description

Labs 3 through 9 plus the two the log-storm work turned up. All blocked on
@lab-attribution-profile, which ranks them by measured upside.

Title cards are fixed. The script enforces the naming rule.

| lab | title card | knob |
|---|---|---|
| 1b | `the-gang-measures-the-measuring-stick` | subscriber on vs off, same fold |
| 1c | `the-gang-survives-the-log-storm` | per-callsite budget, ring drain, silence gap |
| 3 | `the-gang-asks-the-pragma-one-last-time` | hoist the `pragma_recursive_triggers` guard off the row clock |
| 4 | `the-gang-stops-talking-in-json` | hidden columns per source column instead of a JSON payload |
| 5 | `the-gang-copies-the-fts5-homework` | buffer in `xUpdate`, flush in `xSync` |
| 6 | `the-gang-goes-through-the-side-door` | `sqlite3_create_function` instead of writing into the vtab |
| 7 | `the-gang-breaks-into-the-shadow-tables` | can a scalar function write shadows under `SQLITE_DBCONFIG_DEFENSIVE` |
| 8 | `the-gang-fires-all-the-triggers` | session extension replaces the trigger fleet |
| 9 | `the-gang-handles-the-whole-in-list` | `set_in_constraint`, read path only |

## Invalidation edges

- @lab-attribution-profile can kill 3 through 9 outright. If the JSON round trip is
  3% of wall time, none of the diffs are worth writing.
- @lab-session-probe failing kills 8.
- 7 failing kills 6.
- 8 succeeding makes 3 and 6 dead code.
- 4 and 5 compose, so 4 runs first or 5's measured gain absorbs it.

## Lab 5 is no longer speculative

FTS5 does it in tree. `docs/2026-09-19-fts5-clock-choices.md` has the sites. Three
questions that were open are now answered by precedent: where to flush (`xSync`),
how to bound the buffer (byte cap, 1 MiB default, tunable), and what to do about
savepoints (flush on savepoint and release, discard on rollback-to).

## Lab 9 got cheaper

`rusqlite` 0.40.2 surfaces `IndexInfo::is_in_constraint` and `set_in_constraint`
(`src/vtab/mod.rs:602-612`) with `InValues` as a `FallibleIterator`. Both sit behind
the `modern_sqlite` feature, which this crate does not enable today.
