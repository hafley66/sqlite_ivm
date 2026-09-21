# Brief: refactor pass 2, per-view statement program

Repo `~/projects/sqlite_ivm`, worktree from `origin/main` (holds pass 1:
`src/1a_relational.rs` names and install, `1b_state.rs`, `1c_materialize.rs`,
`1d_drain.rs`). Branch `refactor/pass-2-statement-program`. Work in `$PWD`.

## Defect

Every drain rebuilds its SQL text with `format!` per node per call, then
`prepare_cached` hashes that text to find the prepared statement. Profile
(`plans/costs/README.md`): 11% of samples building text, 20% hashing it.
The text depends only on the plan and the view name, never on the row.

## Goal

A `Program` built once per (view, connection), holding every SQL string the
drain path issues, keyed by node id and role. `drain` and everything it calls
(`upsert`, `apply_state`, `split_side`, `fixpoint`, `prepare_scratch`) take
`&Program` and never call `format!` on a hot path. `prepare_cached` stays;
its input is now a `&'static`-like string owned by the `Program`, so hashing
cost falls with text length only where text shortens; the measured win is
the `format!` removal. If hashing still dominates after, stop and report the
profile; do not invent a second cache.

## Shape

- `src/1e_program.rs`: `pub(crate) struct Program { by_node: Vec<NodeStatements>, ... }`,
  `impl Program { pub(crate) fn build(plan: &Plan, name: &str) -> Program }`.
  `NodeStatements` is an enum or struct per `Kind` with named `String`
  fields; no `HashMap<String, String>`.
- Build site: where the vtab connects the plan (`src/2_vtab.rs`, the
  `Connect`/`Create` path that already calls into relational maintenance).
  Stored beside the `Plan` in `Table`. Read `2_vtab.rs:139-465` first.
- Materialize (`1c_materialize.rs`) is once per view; leave it alone this
  pass unless a string is shared with drain, then it reads the `Program` too.
- `format!` that remain on the drain path must be row-dependent (a bound
  parameter is the right answer for those; `?N` placeholders exist already,
  see `parameters()` in `1a_relational.rs`).

## Owned files

`src/1a_relational.rs`, `src/1d_drain.rs`, `src/1e_program.rs` (new),
`src/2_vtab.rs`, `src/lib.rs`, `src/1c_materialize.rs` (only for shared
strings). Forbidden: everything else. Tests unchanged; if a test must change
the pass is wrong.

## Receipts

1. Before, on `origin/main`: `HAFLEY_LOG=sqlite=debug cargo test --release --test 8_group_limit -- --nocapture 2>&1 | grep -o 'time.busy=[0-9.]*[a-zµ]*'`
   summed per run, three runs; same for `13_statements_per_drain`. Then
   after, three runs. Land only if all three after fall below all three
   before on `8_group_limit`. Paste the six numbers per leg in the PR body.
2. `grep -c 'format!' src/1d_drain.rs` before and after in the PR body; the
   remaining sites listed with a one-line reason each (row-dependent).
3. `cargo test` three runs, 75 passing, none new, none changed.
4. `tests/13_statements_per_drain.rs` green: counts unchanged.
5. `cargo clippy -- -D warnings` clean.
6. `git diff --stat origin/main...HEAD` only owned files.

Commit per node kind migrated (Map, Set, Join, Group, Fixpoint), each green.

## Laws

- Every loop bounded; `eprintln!` never; comments constraints only; no
  narrative. Vocabulary: products and rows. No em dashes.
- 10-second law; battery and measurements in the background to a file.
- Noise overlap is never a gain. Do not edit numbers.

## Report

`boop beep --no-wait --as refactor/pass-2-statement-program sprefa-coordinator "<one line>"`
at the PR: number, before/after `time.busy`, `format!` count before/after.
Yield on any question; do not expand.
