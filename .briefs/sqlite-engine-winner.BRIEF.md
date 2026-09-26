# sqlite-engine-winner (research; no product edits)

Goal: write plans/2026-09-26-sqlite-engine-winner.md comparing crates/frontier-engine vs labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow/src/3_sqlite.rs. The user picks the winner; it becomes crates/ivm-sqlite.

Read fully: frontier-engine lib.rs plan.rs engine.rs catalog.rs composition.rs; lab 0_ir.rs 1_rel.rs 3_sqlite.rs HYPOTHESIS.md; src/3_extension.rs (consumer).
Table rows: ops supported (Map Filter Join Antijoin Reduce Distinct LetRec TopK Window Delay), recursion strategy, state tables, indexes, reattach/persist, measured churn (run lab examples/0_paired only if it builds in <10 min), test count, external path deps (frontier-engine Cargo.toml points into hafley-rs-wt/main-codex-attribution). Every cell carries file:line.
Sections after the table: recommendation with evidence; port list (what the loser has that the winner lacks).
Validation: file exists; every cell has file:line.
Commit subject: "plans: sqlite engine winner comparison" with Boop-Status done.

## Laws
- Work only in $PWD (your worktree). Never cd to a primary checkout. Never push. Never commit on main.
- RAM: CARGO_BUILD_JOBS=4, cargo -j 4, RUST_TEST_THREADS=4, CARGO_INCREMENTAL=0. Stop if your process tree exceeds 4GB RSS.
- CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/<your-lane-id>/target. Never $PWD/target.
- Never edit expected/golden/fixture files or flip probe rc to make a test pass. If an expectation looks wrong, commit with Boop-Status blocked + Boop-Ask.
- Commit trailers: Boop-Status wip|done|blocked, Boop-Check "<cmd> -> <result>".
- Final message receipt: status, sha, files, validation, next.

