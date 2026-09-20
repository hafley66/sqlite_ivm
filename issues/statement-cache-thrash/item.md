---
created: 2026-09-20
updated: 2026-09-20
type: improvement
status: open
priority: high
epic: lab-queue-round-one
lane: lab-cache
lane_seq: 10
labels: [perf]
collision: [src/1_maintenance.rs, src/1a_relational.rs]
---

# Statement cache thrashing: the maintenance path re-prepares every row

**Half shipped in #15**: the seven sites in `src/1_maintenance.rs` use `prepare_cached`,
229.3 ms to 59.6 ms on the canonical corner. The 19 sites in `src/1a_relational.rs`
remain, and are now unblocked: the fixpoint lane that owned that file landed in #16.

## Description

The attribution profile measured the fold two ways and the gap is the whole story.

| prepare policy | fold, 2048 changes |
|---|---|
| re-preparing the maintenance statements per row | 163.2 ms |
| reusing them | 40.6 - 41.4 ms |

**Re-preparing is about 75 percent of the fold.** Every other queued lab prices at
6 percent or less. Source:
`labs/20260920.1.the-gang-finds-out-where-the-time-went/HYPOTHESIS.md`, verdict 2026-09-20,
corner 8 tables x 20 columns, join arity 4, seed 42, median of three legs.

## Where it happens, at origin/main

`maintain()` in `src/1_maintenance.rs` builds each statement with `format!` and runs it
through `db.execute(&sql, ...)` or `db.query_row(&sql, ...)`. Both prepare a fresh
statement on every call, then drop it. Per maintained row.

| file | fresh-prepare call sites |
|---|---|
| `src/1_maintenance.rs`, the `maintain` path | 7 |
| `src/1a_relational.rs` | 19 |

## The idiom already exists here

`rusqlite` keeps an LRU of prepared statements keyed by SQL text, and this repo already
uses it in seven places:

- `src/1a_relational.rs:26`, `:69`, `:133`, `:217`, `:871`
- `src/2_vtab.rs:138`, `:302`

The SQL text on the maintenance path is stable per view per step, so the cache hits.
The strings are rebuilt with `format!` each call, which costs an allocation but does not
defeat the cache; hoisting the `format!` is a second, smaller win.

## Why this is not lab 5

`lab-batch-at-xsync` changes *when* the work runs: buffer in `xUpdate`, flush in `xSync`.
This issue changes *how each statement is prepared* and touches no control flow. They
compose, and this one is far smaller. The profile's cached-mode leg measured exactly this
change, so the ceiling is known before any code moves.

## Measure by count, not by clock

Other agents may hold the machine. The claim is a prepare count, not a duration:
`hafley-observe`'s `CountRecorder` over `sqlite.statement`-class spans, before and after.

## Acceptance Criteria

- [x] every statement on the `maintain` path goes through `prepare_cached`
- [ ] the same for the 19 sites in `src/1a_relational.rs` that do not already
- [ ] a `CountRecorder` assertion pins prepares per maintained row and it drops
- [ ] the cache is bounded, and the bound is stated with what it protects
- [ ] a test fails if a new fresh-prepare call site appears on the maintenance path
- [ ] the existing battery is green and unchanged
