# Brief: refactor pass 2b, land the statement program

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`refactor/pass-2b-statement-program`. Work in `$PWD`.

## Situation

A previous lane wrote the pass-2 change and exited without committing. Its
tree is at `/Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/refactor/pass-2-statement-program`
(read only for you; never edit or commit there). Graded facts on that tree:
builds; `format!` in `src/1d_drain.rs` 64 -> 1; `src/1e_program.rs` 570
lines; no failing test. Unknown: whether every test binary ran, and the
before/after timing.

Read `plans/unify/refactor-2.brief.md` first: it is the design and the
receipts; this brief only changes the start.

## Step 1: take the tree

```
git -C /Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/refactor/pass-2-statement-program diff > /tmp/pass2.patch
git -C /Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/refactor/pass-2-statement-program status --short
cp .../src/1e_program.rs src/1e_program.rs     (untracked file, not in the diff)
git apply /tmp/pass2.patch
cargo build
git add src && git commit -m "relational: statement program built once per view (wip, unmeasured)"
```

## Step 2: receipts, exactly as `refactor-2.brief.md` lists them

The before side runs on `origin/main` in a sibling worktree you create with
`git worktree add ../pass-2b-baseline origin/main` (same depth as this one so
the relative path deps in `Cargo.toml` resolve; use
`CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/refactor-pass-2b/baseline-target`
for it). Delete that worktree when done (`git worktree remove`).

Timing receipt, both sides, three runs each:
`HAFLEY_LOG=sqlite=debug cargo test --release --test 8_group_limit -- --nocapture 2>&1 | grep -o 'time.busy=[0-9.]*[a-zµ]*'`
summed per run in seconds. Land only if all three after fall below all three
before. If they do not, report the six numbers and stop; do not tune.

Battery receipt: `cargo test` three runs, 75 passed each (count the
`test result` lines: there are 17 binaries on `origin/main`; if fewer run,
say which are missing and why).

## Owned files

Same as `refactor-2.brief.md`. Plus `plans/costs/statement-program.md` for
the numbers.

## Laws

Same as `refactor-2.brief.md`. Disk is at 19G free: one build dir, no
extra target dirs beyond the two named above, `cargo clean` both before you
exit.

## Report

`boop beep --no-wait --as refactor/pass-2b-statement-program sprefa-coordinator "<one line>"`
at the PR or at the stop: PR number or "stopped", the six timing numbers,
battery counts.
