# ivm-crate-promotion phase A (large)

Full issue: `issuectl show ivm-crate-promotion` (run in ~/projects/sprefa, read-only). Design: plans/2026-09-24-ivm-cousins/4_design.md §2.4, §4.
User decision 2026-09-26: crates/frontier-engine is the ivm-sqlite base. Comparison + port list: plans/2026-09-26-sqlite-engine-winner.md (on branch chore/sqlite-engine-winner, commit 61ee692; read with git show 61ee692:plans/2026-09-26-sqlite-engine-winner.md).

LAB = labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow

## Goal
Make the workspace crates below, in this commit order. Each commit builds and its moved tests are green.
1. crates/ivm-ir: LAB src/0_ir.rs (data only: Program, Op, Expr, Agg, ...). Move with git mv, keep numeric file prefixes.
2. crates/ivm-engine: LAB src/1_rel.rs (trait Engine, trait Rel, fn lower) + new trait Host with Raw and Plugin impls per 4_design.md §2.4.
3. crates/ivm-dd: LAB src/2_dd.rs, impl Engine.
4. crates/ivm-sqlite: git mv crates/frontier-engine -> crates/ivm-sqlite (keep catalog-in-db, reattach, composition; src/3_extension.rs keeps working). Add impl Engine for it by porting LAB src/3_sqlite.rs lowering: Map, Filter, Antijoin, Distinct, TopK, Reduce Min/Max, DRed LetRec (keep lab's documented rejections). One commit per ported op group.
5. LAB tests/, oracle/, examples/ move to the crates that own them. Lab suites (scripts, random 1000 seeds, DD gates, alloc gate) run against ivm-dd AND ivm-sqlite from the new crates. Delete the LAB dir only after its suites are green from the new home.
Out of scope: ivm-sql-frontend, Op::Mint, Window, Delay, sprefa changes.

## Constraints
- No absolute path deps into hafley-rs worktrees (frontier-engine Cargo.toml:9-11 points into hafley-rs-wt/main-codex-attribution). Point sqlite-ext and hafley-observe at hafley-rs main via a path that works from a boop worktree: add a `boop-start` just recipe that symlinks ~/projects/hafley-rs into the worktree (copy the pattern of sprefa's justfile boop-start), and path-dep through that symlink. hafley-rs main now has hafley-observe Growth::Log (e55fee6e).
- Fix the code, never the expected outputs. Frontier-engine's 23 tests and root crate tests stay green.
- If porting an op into frontier-engine's plan shape needs a design call (e.g. state-table layout collides), commit Boop-Status blocked with Boop-Ask.

## Validation
cargo test -j 4 --workspace; plus the lab random suite at 1000 seeds against both engines. Put the exact commands + result lines in Boop-Check.

## Laws
- Work only in $PWD (your worktree). Never cd to a primary checkout. Never push. Never commit on main.
- RAM: CARGO_BUILD_JOBS=4, cargo -j 4, RUST_TEST_THREADS=4, CARGO_INCREMENTAL=0. Stop if your process tree exceeds 4GB RSS.
- CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/<your-lane-id>/target. Never $PWD/target.
- Commit trailers: Boop-Status wip|done|blocked, Boop-Check "<cmd> -> <result>".
- Final message receipt: status, sha, files, validation, next.
