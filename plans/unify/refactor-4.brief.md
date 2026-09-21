# Brief: refactor pass 4, split the compiler by clause

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`refactor/pass-4-compiler-split`. Work in `$PWD`. Commit before measuring;
never exit with a dirty tree.

## Goal

`src/0b_relational.rs` (2173 lines; `impl Compiler` at 637-1809, 1170
lines) becomes files by clause. Pure move: no logic change, no SQL string
change, no pub rename used outside the file.

Read the whole `impl Compiler` first and write the split table into the
first commit as `plans/unify/pass-4-map.md`: every method, its line range,
and its target file. Target files, adjust names to what the methods are:

| file | takes |
|---|---|
| `src/0b_relational.rs` | `Kind`, `Field`, `Rule`, `Node`, `Plan`, `Source`, `Occurrence`, the free helpers (`sql`, `name`, `alias`, `resolve`, collation fns, `key_expression`, `key_sql`, `expression*`, `affinity*`, `has_aggregate`, `ordinal`, `integer_limit`, `direction`, `nulls`), `bind` (2008) |
| `src/0c_compile_from.rs` | `impl Compiler`: FROM, joins, table mentions (`table_mentions`, `from_mentions`, `part_mentions`, `select_mentions`, `equalities`, `column_pair`, `conjuncts`) |
| `src/0d_compile_select.rs` | `impl Compiler`: projection, WHERE, GROUP BY, window, ORDER/LIMIT |
| `src/0e_compile_recursive.rs` | `impl Compiler`: WITH RECURSIVE (`recursion_shape`, `where_keys_for_step`, rule building) |
| `src/0f_columns.rs` | `visit_columns`, `column_references`, `renumber_columns` (2111-2170) |

Also: the `Field { .. }` literal built four times (at 791, 1309, 1337,
1403 before the split) becomes one constructor fn; the four sites call it.
That is the only non-move change.

Multiple `impl Compiler` blocks across files are fine. Private helpers a
moved fn needs become `pub(crate)`. `lib.rs` gets `#[path]` mod lines in
the existing pattern.

## Owned files

`src/0b_relational.rs`, `src/0c_compile_from.rs`, `src/0d_compile_select.rs`,
`src/0e_compile_recursive.rs`, `src/0f_columns.rs`, `src/lib.rs`,
`plans/unify/pass-4-map.md`. Forbidden: everything else, tests included.

## Receipts

1. `cargo test` three runs: 68 passed, 15 binaries, as `origin/main`.
2. SQL and format strings unchanged:
   `git show origin/main:src/0b_relational.rs | grep -o 'format!(\s*"[^"]*"' | sort > /tmp/b.txt`;
   `cat src/0b_relational.rs src/0c_*.rs src/0d_*.rs src/0e_*.rs src/0f_*.rs | grep -o 'format!(\s*"[^"]*"' | sort > /tmp/a.txt`;
   `diff /tmp/b.txt /tmp/a.txt` empty; paste it.
3. `tests/13_statements_per_drain.rs` counts unchanged.
4. `cargo clippy -- -D warnings` clean.
5. No file over 700 lines: `wc -l src/0*.rs`.
6. `git diff --stat origin/main...HEAD` only owned files.
7. Release wall `8_group_limit` three runs each side: not slower.

Commit per file extraction. PR against main with the seven receipts.

## Laws

Comments: move with their code, add none. `eprintln!` never. Every loop
keeps its budget. No em dashes. 10-second law; battery in the background
to a file. One target dir (set for you); `cargo clean` any baseline
worktree before exit.

## Report

`boop beep --no-wait --as refactor/pass-4-compiler-split sprefa-coordinator "<one line>"`
at the PR: number, receipts met, receipts missed with text.
