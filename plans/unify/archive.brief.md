# Brief: archive labs, probes, crud scripts; close lab issues

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`chore/archive-labs`. Work in `$PWD` only. One commit, one PR.

## Owned files

`labs/**`, `probes/**`, `scripts/1_crud.sh`, `scripts/2_join_crud.sh`,
`scripts/3_composite_join_crud.sh`, `scripts/4_lifecycle.sh`,
`scripts/5_vtab_ddl.sh`, `scripts/6_source_ddl.sh`, `scripts/9_verify.sh`,
`issues/lab-*/item.md`, `issues/lab-queue-round-one/item.md`,
`plans/2026-09-20-lab-*.brief.md`, new `archive/README.md`.
Forbidden: `src/`, `tests/`, `bench/`, `Cargo.toml`, everything else.

## Steps

1. `git mv labs archive/labs`; `git mv probes archive/probes`;
   `git mv plans/2026-09-20-lab-*.brief.md archive/plans/`.
2. For each of the seven scripts: grep the repo for its name
   (`grep -rn "<name>" --include=*.md --include=*.sh --include=*.toml --include=*.rs --include=*.mjs .`).
   If a reference exists outside `archive/`, `chat_log/`, `plans/`, leave the
   script in place and list it in the report. Else `git mv` it to
   `archive/scripts/`.
3. `archive/README.md`, 5 lines max: what is here, why (labs die on landing
   by repo law; crud smoke scripts superseded by `tests/`), nothing in
   `archive/` runs in CI.
4. Close issues `lab-attribution-profile`, `lab-batch-at-xsync`,
   `lab-drop-json-payload`, `lab-session-probe`, `lab-wide-table-harness`,
   `lab-queue-round-one`. `issuectl close --help` for the exact form; if no
   reason flag exists, `issuectl note <slug> "archived: labs die on landing"`
   after the close.
5. `CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/chore-archive-labs/target cargo test --test 0_query`
   passes.
6. Commit subject `chore: archive labs, probes, crud scripts; close lab issues`,
   trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
7. Push; `gh pr create --base main`, body three lines plus
   `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

## Report

`boop beep --no-wait --as chore/archive-labs sprefa-coordinator "<one line>"`:
PR url, files moved, files left in place with the referencing path.
