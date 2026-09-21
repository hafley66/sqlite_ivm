# Brief: refactor pass 3, one engine behind the vtab

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`refactor/pass-3-one-engine`. Work in `$PWD`.

## Fact

Two maintenance engines sit behind one virtual table. `src/0_query.rs`
(372 lines, a `Query` of filters and join keys) plus `src/1_maintenance.rs`
(411 lines, per-row trigger contributions) is the older path.
`src/0b_relational.rs` plus `1a`..`1e` is the relational plan. `2_vtab.rs`
imports both (`maintenance` and `relational_maintenance`); `2a_source_ddl.rs`
calls both `query::bind` and `relational::bind`. Read `2_vtab.rs:139-465`
to find the branch that picks one.

## Step 1: the shape table (no code change, first commit)

`plans/unify/pass-3-shapes.md`: one row per test in `tests/0_query.rs` and
`tests/1_maintenance.rs` (and any other test that reaches the old path;
grep for `maintenance::` and `query::bind` in `tests/`). Columns: test
name, the SQL shape it exercises, which engine serves it today, whether
`relational::bind` accepts that SQL (run it: a tiny probe test or a
`dl`-style one-off under `tests/`, deleted before the PR). Commit the table.

## Step 2: decide by the table

- Every shape accepted by the relational plan: proceed to step 3.
- Any shape not accepted: stop here. PR with the table only, and the
  report names the shapes. The parent decides whether the relational plan
  grows or the old path stays.

## Step 3: retire

Route every view through the relational plan. Delete `src/0_query.rs` and
`src/1_maintenance.rs` and their imports; `0a_catalog.rs` keeps what the
relational path uses. Move each test in `tests/0_query.rs` and
`tests/1_maintenance.rs` whose assertion still holds to the relational
test files (`tests/3_relational.rs` or a new `tests/1_maintenance.rs`
rewritten against the vtab); keep its name and its assertion text. A test
whose assertion is about the old engine's internals (trigger bodies,
`contributions` SQL) is deleted with a one-line reason in the PR body.
Storage format: if the old path wrote a different on-disk shape, the
`migrate` fn in `2_vtab.rs:75` must carry it; test it with a db created on
`origin/main` (build the baseline in a sibling worktree, same depth).

## Owned files

`src/**`, `tests/0_query.rs`, `tests/1_maintenance.rs`, `tests/3_relational.rs`,
`tests/2_vtab.rs`, `plans/unify/pass-3-shapes.md`. Forbidden: `bench/**`,
other tests, docs.

## Receipts

1. The shape table, every row with a yes/no from an actual bind call.
2. `cargo test` three runs: total passing equals `origin/main` (75) minus
   deleted internals tests plus moved tests; the arithmetic in the PR body.
3. `tests/13_statements_per_drain.rs` counts unchanged.
4. `wc -l src/*.rs` before and after.
5. `cargo clippy -- -D warnings` clean.
6. Release wall for `8_group_limit` and `4_features` three runs each side;
   not slower (all three after at or below the before max).
7. `git diff --stat origin/main...HEAD` only owned files.

## Laws

Every loop bounded; `eprintln!` never; comments constraints only; no
narrative. Vocabulary: products and rows. No em dashes. 10-second law;
battery in the background to a file. Disk: one target dir
(`CARGO_TARGET_DIR` is set for you), `cargo clean` the baseline before exit.

## Report

`boop beep --no-wait --as refactor/pass-3-one-engine sprefa-coordinator "<one line>"`
at step 2 (table done, proceed or stop) and at the PR.
