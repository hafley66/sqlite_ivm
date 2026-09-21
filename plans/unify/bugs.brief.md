# Brief: every open bug, one lane, one PR

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch `fix/open-bugs`.
Work in `$PWD` only. Commit after each numbered item; scoped, green.

## Owned files

`src/**`, `tests/**`, `docs/failure-modes.md`, `issues/**` (frontmatter and
acceptance boxes only). Forbidden: `bench/**`, `scripts/**`, `plans/**`,
`labs/**`, `probes/**`, `Cargo.toml` deps.

## Laws

- Measure before and after with the engine's own spans:
  `HAFLEY_LOG=sqlite=debug cargo test --release --test <leg> -- --nocapture`
  and `time.busy` on close events. Three runs each side; a change counts only
  when all three after-runs fall below all three before-runs. Never call a
  noise overlap a gain.
- Every loop bounded with a named diagnostic. `eprintln!` never.
- Comments: constraints only. No narrative, no dates, no arc names.
- Vocabulary: products and rows. Banned words: provenance, substrate,
  load-bearing, regime, ground truth. No em dashes.
- `cargo test` at the root is the battery. It is green on `origin/main`
  (72 legs). `tests/14_scale.rs` is slow; run it only by name.
- 10-second law: nothing over 10s in the foreground; long tests in the
  background to a file.

## Items, in order

### 1. `tests/14_scale.rs` leaves the battery

Add `#[ignore = "scale sweep, run by name: cargo test --release --test 14_scale -- --ignored"]`
to its one test. Receipt: `cargo test` wall under 60s.

### 2. window LIMIT drops a multiplicity copy (`issues/window-limit-drops-multiplicity-copies`)

Read the issue body. Fail-pre-fix test: `tests/8_group_limit.rs:78`
`window_with_limit_reads_every_copy`, `#[ignore]`. Site: `src/1a_relational.rs`
`Kind::Group` branch taken when `*window || limit.is_some()`; two builders of
the `expanded` CTE, incremental and bulk. Fix the bag, remove the `#[ignore]`.
Receipts: that test green; the three window shapes green under all eight
multiplicity shapes; a `docs/failure-modes.md` row (incident, cause,
fail-pre-fix test, rail); acceptance boxes checked in the issue.

### 3. group LIMIT growth assertion (`issues/group-cte-unrolls-multiplicity`)

`hafley_observe::assert_growth` exists in the path dep at `Cargo.toml:25`
(`crates/hafley-observe/src/4_counts.rs:180`). Read `tests/10_growth.rs` for
the pattern already in use. Add a test that pins Group entry count as
Constant in input multiplicity for the `limit` branch. Check the last box,
close the issue with `issuectl close`.

### 4. json subtype ledger row (`issues/json-subtype-group-key-splits-groups`)

Fix is in tree: `tests/7_key_agreement.rs:290`
`json_subtype_group_key_agrees_between_bulk_and_incremental_paths`. Owed: the
`docs/failure-modes.md` row and the boxes. Verify the first box against the
code before checking it. Close the issue.

### 5. TEXT keys still in the write path (`issues/json-text-keys-in-indexes`)

Most of this landed: `src/1a_relational.rs:211` arrangement tables carry
`__k INTEGER, __r INTEGER`; `:244` is the dictionary. Remaining TEXT keys on
the write path:

| site | column | what it is |
|---|---|---|
| `src/1a_relational.rs:193` | `_state` `__key TEXT` + index | per-view state |
| `:859` | scratch `__v TEXT` | fixpoint scratch |
| `:878` | `__k TEXT PRIMARY KEY` | find which table |

For each: decide whether it is on the per-write path (a statement issued per
drain) or once per view. Per-write sites move to INTEGER through the existing
dictionary. Once-per-view sites get a one-line note in the issue and stay.
Each moved site: measure `8_group_limit` and `13_statements_per_drain`
`time.busy` three runs before and after, commit with the numbers in the
message. Then check the boxes that hold and close the issue; if a box cannot
hold, write why under it.

### 6. Issue hygiene

`issuectl close dylib-entrypoint-untested` (all boxes checked, test in tree
`tests/6_extension_load.rs`). `issuectl close statement-cache-thrash` if
`origin/main` shows it `done` already, skip.

## Receipts for the PR body

- battery: `cargo test` three runs, pass counts and wall each
- `8_group_limit` and `13_statements_per_drain` `time.busy` before and after
  item 5, three runs each
- `git diff --stat origin/main...HEAD` lists only owned files
- per item: commit sha

## Report

`boop beep --no-wait --as fix/open-bugs sprefa-coordinator "<one line>"` at
the PR: PR number, items done, items yielded with the exact error text.
Yield on any scope question; do not expand.
