# Brief: refactor pass 3b, land the one-engine retire

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`refactor/pass-3b-one-engine`. Work in `$PWD`.

## Situation

The pass-3 lane proved every old-path shape binds through the relational
plan (its table: `plans/unify/pass-3-shapes.md` at commit `9aebf80` on
branch `refactor/pass-3-one-engine`) and wrote the retire, then exited
without committing. Its tree is at
`/Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/refactor/pass-3-one-engine`
(read only; never edit or commit there). 18 dirty files, one build error
outstanding, plus an untracked `tests/zz_probe.rs` that must not land.

Read `plans/unify/refactor-3.brief.md` first: design, receipts, laws.

## Step 1: take the tree

```
git cherry-pick 9aebf80
git -C /Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/refactor/pass-3-one-engine diff > /tmp/pass3.patch
git apply /tmp/pass3.patch
git status --short         (expect the 17 tracked changes; no zz_probe.rs)
cargo build                (fix the one error; keep the fix minimal)
git add -A src tests && git commit -m "relational: retire the trigger engine (wip)"
```

Commit before anything else. Never exit with a dirty tree; if stuck, commit
what builds and report.

## Step 2: receipts from `refactor-3.brief.md` steps 3 and the list

Then PR. Owned files and laws as in `refactor-3.brief.md`, plus
`plans/unify/pass-3-shapes.md`.

## Report

`boop beep --no-wait --as refactor/pass-3b-one-engine sprefa-coordinator "<one line>"`
after the wip commit and at the PR.
