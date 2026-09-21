# Brief: departure fixpoint, set-at-a-time

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`perf/departure-set-at-a-time`. Work in `$PWD`. Commit wip the moment it
builds; never exit with a dirty tree. Measurement logs go under
`$CARGO_TARGET_DIR/logs/`, never `/tmp`.

## The defect, measured (`plans/costs/toward-dd/README.md`)

`reach` fanout 1 n 100000: sqlite-ivm 644 s per cell, dd 7.8 ms. Profile is
92% SQLite row scans. Derive rounds stay flat at 161-207 (one-hop diameter).
Departure (delete) rounds: 5795 at n 10000, 343027 at 16000, 572824 at 32000,
52236 at 40000. Each round scans the member table (~10 ms at n 100000), so
one delete costs ~500 scans. Cause, in `src/1d_drain.rs` `fixpoint` departure
pass and the SQL in `src/1e_program.rs`: the departing set advances one hop
per round over a work rowid range, and the join predicate casts both sides so
no index serves it.

## Goal

Departure closes set-at-a-time, the way derive already does (PR #21: arrived,
left, deleted scratch tables; one statement per round over the whole work
set; `BULK_*_BUDGET` constants with named diagnostics). Read the derive side
first and mirror its shape. Two stacked changes, each measured on its own:

1. Rounds: the departure pass derives from the whole current departing set
   per round (a recursive CTE or a scratch-table loop over sets), not from a
   rowid slice. Round count for `reach` n 40000 must drop from 52236 to the
   order of the derive rounds (hundreds). Print the count with
   `RUST_LOG=sqlite_ivm=debug` before and after, put both in the ledger.
2. Scan: the join predicate serves an index. Remove the cast on the indexed
   side or add a computed index; `EXPLAIN QUERY PLAN` before and after in the
   ledger showing `SEARCH` where it said `SCAN`.

Not a storage format bump. Not a change to `__k` typing (that was measured
and rejected, `issues/json-text-keys-in-indexes`). If either change needs a
format bump, stop and report.

## Land rule

`bench scale --circuits reach --n 10000,100000 --fanout 1,10 --reps 3 --arms sqlite-ivm`
before (banked on main in the README's "arc 3 landing baseline") and after.
All three after-runs below all three before-runs on every one of the four
cells. Battery `cargo test` three runs, 68 passed, 15 binaries.
`tests/13_statements_per_drain.rs`: the recursive counts may go down; if they
change, update the pinned numbers in that test with the new value and say so.
Retraction correctness: `tests/9_fixpoint_retraction.rs` and
`tests/4_features.rs` green. `cargo clippy -- -D warnings` clean.

## Owned files

`src/1d_drain.rs`, `src/1e_program.rs`, `src/1a_relational.rs` (budgets),
`tests/13_statements_per_drain.rs` (pinned counts only),
`plans/costs/toward-dd/README.md` (append), `docs/failure-modes.md` (one
row). Forbidden: everything else.

## Laws

Every loop bounded with a named diagnostic. `eprintln!` never. Comments:
constraints only. No em dashes. Three runs each side, noise is never a gain,
never edit a number. 10-second law: sweeps in the background to a file; one
bench process at a time, `nice -n 10`.

## Report

`boop beep --no-wait --as perf/departure-set-at-a-time sprefa-coordinator "<one line>"`
after the wip commit, after change 1 measured, after change 2 measured, at
the PR. Each line: round counts before/after, the four cells before/after.
