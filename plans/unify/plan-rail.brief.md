# Brief: no full scan on the maintenance path, proven per statement

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`rail/no-scan-plans`. Work in `$PWD`. Commit wip the moment it builds; never
exit with a dirty tree. Logs under `$CARGO_TARGET_DIR/logs/`.

## Law (user, 2026-09-21)

Every SQL statement the engine issues on the maintenance path has its plan
checked before it ever runs. A `SCAN` is a defect unless it is named as an
exception with a reason. The exceptions are proven, not assumed.

## Where

`src/1e_program.rs` `Program::build` holds every drain-path statement for a
view (built once per view and connection, PR #28). Materialize statements in
`src/1c_materialize.rs` are once per view; include them if they route through
`Program`, else add a second check at their build site.

## Steps

1. `Program::build` (or a `Program::check(db)` called right after it, at the
   same site in `src/2_vtab.rs`) runs `EXPLAIN QUERY PLAN` on every statement
   it holds. Read `bind` in `src/0b_relational.rs:610-640` for the existing
   `EXPLAIN` walk and match its style.
2. A plan line whose `detail` starts with `SCAN` fails with
   `error("full scan in <statement name>: <detail>")` unless the statement
   name is in `SCAN_ALLOWED: &[(&str, &str)]` (name, reason). Scratch and
   temp tables that are always small are the expected members; each entry
   carries the constraint that keeps it small (a budget constant name or a
   row-count bound). A `SCAN` of a `CONSTANT ROW` or of a `USING COVERING
   INDEX` is not a full scan; read the SQLite `EXPLAIN QUERY PLAN` docs for
   the exact detail strings before writing the match.
3. Test `tests/15_no_scan_plans.rs`: for every circuit query in
   `bench/src/fixture.rs` (read it, do not import it) and every view in
   `tests/fixtures/0_shared.json`, create the view on a fresh db with the
   source tables indexed the way the bench indexes them, and assert the
   build succeeds. A second test lists every allowlist hit with its statement
   name and pins the count. If the departure join (`perf/departure-set-at-a-time`,
   merged before you start) still scans, that is a failing test, not an
   allowlist entry; report it.
4. The statement span on `HAFLEY_LOG=sqlite=debug` gains a `plan` field with
   the first `EXPLAIN QUERY PLAN` line, so a profile can group by plan.
5. `docs/failure-modes.md` row: the departure full scan, how it hid, this rail.

## Owned files

`src/1e_program.rs`, `src/2_vtab.rs` (the call site only), `src/1c_materialize.rs`
(only if step 1 needs it), `tests/15_no_scan_plans.rs`, `docs/failure-modes.md`.
Forbidden: everything else.

## Receipts

1. `cargo test` three runs: 68 + the new tests, 16 binaries.
2. The allowlist printed in the PR body, every entry with its reason.
3. `tests/13_statements_per_drain.rs` counts unchanged (EXPLAIN runs at
   build, not per drain; prove it with the rail's own count).
4. Release wall `8_group_limit` three runs each side: not slower.
5. `cargo clippy -- -D warnings` clean.

## Laws

Every loop bounded; `eprintln!` never; comments constraints only; no em
dashes; vocabulary products and rows; 10-second law.

## Report

`boop beep --no-wait --as rail/no-scan-plans sprefa-coordinator "<one line>"`
at the wip commit and at the PR: allowlist size, any SCAN the rail found.
