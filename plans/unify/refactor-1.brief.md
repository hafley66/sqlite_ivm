# Brief: refactor pass 1, split `src/1a_relational.rs` by phase

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`refactor/pass-1-split-relational-maintenance`. Work in `$PWD` only.

## Goal

`src/1a_relational.rs` (1439 lines, `impl Plan` 182-1362) becomes four
files. Pure move: no SQL text changes, no logic changes, no renames of pub
items used outside the file.

| new file | takes | from lines |
|---|---|---|
| `src/1a_relational.rs` | names, hash, intern, `Role`, `rule_from`, `rule_where`, `CachedExecute`, budgets, `install`, `hooks` | 1-181, 1363-1439 |
| `src/1b_state.rs` | `impl Plan { create_state, populate, exhaust, key_sql, fill, source_table, write_state, validate, collector }` | 183-451, 796-845 |
| `src/1c_materialize.rs` | `impl Plan { materialize, derive }`; each `Kind` arm of the `materialize` match (456-768) becomes one private fn `materialize_input`, `materialize_map`, `materialize_set`, `materialize_join`, `materialize_group`, `materialize_fixpoint`, the match calls them | 452-795 |
| `src/1d_drain.rs` | `impl Plan { prepare_scratch, drain, upsert, apply_state, split_side, fixpoint }`; each arm of the `drain` match (930-997) becomes one private fn the same way | 846-1362 |

`lib.rs` gets three `#[path]` mod lines matching the existing pattern.
Multiple `impl Plan` blocks across files are fine; `Plan` stays in
`0b_relational.rs`. Private helpers a moved fn needs become `pub(crate)`.

## Owned files

`src/1a_relational.rs`, `src/1b_state.rs`, `src/1c_materialize.rs`,
`src/1d_drain.rs`, `src/lib.rs`. Forbidden: everything else, tests included.

## Laws

- Comments: constraints only. Move existing comments with their code; add
  none that narrate the move.
- `eprintln!` never. Every loop keeps its budget constant and its diagnostic.
- No format string changes. Receipt 3 proves it.
- 10-second law; battery in the background to a file.

## Receipts

1. `cargo test` three runs: same pass count as `origin/main`, every leg green.
2. `cargo test --test 13_statements_per_drain` green; it pins statement counts.
3. SQL text unchanged: `git show origin/main:src/1a_relational.rs | grep -o 'format!(\s*"[^"]*"' | sort > /tmp/before.txt`;
   `cat src/1a_relational.rs src/1b_state.rs src/1c_materialize.rs src/1d_drain.rs | grep -o 'format!(\s*"[^"]*"' | sort > /tmp/after.txt`;
   `diff /tmp/before.txt /tmp/after.txt` empty. Paste the empty diff line in the PR body.
4. `cargo clippy -- -D warnings` clean.
5. No file over 700 lines: `wc -l src/1*.rs`.
6. `git diff --stat origin/main...HEAD` lists only owned files.

Commit per file extraction, four commits minimum. PR against main, body
carries the six receipts.

## Report

`boop beep --no-wait --as refactor/pass-1-split-relational-maintenance sprefa-coordinator "<one line>"`:
PR number, receipts met, receipts missed with exact text. Yield on any
question; do not expand.
