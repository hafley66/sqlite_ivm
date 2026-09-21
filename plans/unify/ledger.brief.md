# Brief: six owed failure-ledger rows

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`docs/failure-ledger-rows`. Work in `$PWD`. Docs only.

## Owned files

`docs/failure-modes.md`. Nothing else. No code, no tests.

## Job

Six incidents bit earlier arcs and never got their row. For each, find the
fix commit (`git log -S'<term>' --oneline`, `git log --grep`), the test that
fails before the fix (grep `tests/` for the term), and the rail that keeps
it from returning (a test name, a budget constant, an assertion). Write one
row per incident in the existing table shape at the top of
`docs/failure-modes.md` (columns: incident, root cause, fail-pre-fix test,
rail). Every cell cites a `path:line`, a commit sha, or a test name; a cell
you cannot cite says `not found: <what you searched>`.

| incident | search terms |
|---|---|
| subtype lost update | `subtype`, `json_type`, `tests/7_key_agreement.rs` |
| `__c` stored twice | `__c`, `double`, `count` in `src/1b_state.rs`, `src/1d_drain.rs` |
| `json_type(value)` returns NULL | `json_type`, `NULL` |
| UNION ALL second input dropped | `union`, `Set("all")`, `tests/4_features.rs` |
| DDL inside a trigger | `DDL`, `trigger`, `src/2a_source_ddl.rs`, `tests/5_transactions.rs` |
| drain clears marks then `ROLLBACK TO` | `ROLLBACK TO`, `savepoint`, `marks`, `tests/5_transactions.rs` |

## Laws

Textbook register, short sentences, present tense. No em dashes. No
narrative. Banned words: provenance, substrate, load-bearing, regime,
ground truth.

## Receipts

- `git diff --stat origin/main...HEAD` shows only `docs/failure-modes.md`.
- Six new rows; each cell cited or marked `not found`.
- `cargo test --test 0_query` unaffected (no code touched); skip the build.

One commit, PR against main, body lists the six rows' first column.

## Report

`boop beep --no-wait --as docs/failure-ledger-rows sprefa-coordinator "<one line>"`
at the PR.
