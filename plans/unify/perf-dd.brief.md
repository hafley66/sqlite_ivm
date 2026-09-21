# Brief: sqlite_ivm against DD, measured, then closed

Repo `~/projects/sqlite_ivm`, worktree from `origin/main`, branch
`perf/toward-dd`. Work in `$PWD` only. PR per arc.

## Yardstick

The bench binary (`bench/`, subcommands `shootout` and `scale`) has a `dd`
arm. Every table this lane produces carries, per circuit per n per arm:
wall ms, peak RSS MiB, disk bytes written, disk bytes read, db size on disk,
arrangement rows. And one derived column `ivm/dd` = sqlite-ivm wall over dd
wall. The job is to push `ivm/dd` down, circuit by circuit, with a profile
before every change and three runs each side after.

## Arc 1: the measurement (no engine change)

1. Confirm the `scale` subcommand reports peak RSS, disk written, disk read
   per arm. If any is missing, add it in `bench/` (owned) with the process
   metrics crate the bench already chose; on macOS `rusage` `ru_maxrss` and
   `proc_pid_rusage` `ri_diskio_byteswritten`. Read `bench/README.md` first.
2. Run `scale` for chain, join, group, distinct, window, reach at n =
   1000, 10000, 100000, fanout 1 and 10, arms sqlite-ivm, sqlite-query, dd.
   Disk WAL. Background, output to `plans/costs/toward-dd/0_baseline.tsv`
   plus the SVGs.
3. Write `plans/costs/toward-dd/README.md`: the table, `ivm/dd` per cell,
   and the three worst cells named with their number.
PR.

## Arc 2..n: one circuit per arc

Take the worst `ivm/dd` cell. `samply record` the release bench binary on
that cell alone (`samply` is installed; `--save-only`, then
`samply load` is not needed; read the JSON with the `profile-json` summary
or the flamegraph's top 10 self-time frames into the ledger). Name the
frame, state a hypothesis with a number, change one thing in `src/`,
measure three runs each side. Land only when all three after-runs fall
below all three before-runs on that cell AND the full battery is green AND
`13_statements_per_drain` counts are equal or lower. Each landed change:
ledger row in `plans/costs/toward-dd/README.md` and `docs/failure-modes.md`
if it was a defect. Each rejected change: one line with the numbers, no
commit in `src/`.

Known suspects from the last profile (`plans/costs/README.md`): SQL text
rebuilt per drain (11%), statement-cache hashing of long SQL (20%), chain
write growing with n. Refactor pass 2 (statement program) may land in
parallel on another lane; if `src/1d_drain.rs` exists on main when you
start an arc, rebase first.

## Owned files

`src/**` (coordinate: check `boop beep lane list` for a live
`refactor/pass-*` lane; if one is alive, do not touch `src/` until it merges,
work on the bench and ledger meanwhile), `bench/**`, `plans/costs/toward-dd/**`,
`docs/failure-modes.md`, `tests/13_statements_per_drain.rs` (counts may go
down, never up).

## Laws

- Three runs each side; noise overlap is never a gain. Never edit a number.
- Every loop bounded; `eprintln!` never; comments constraints only.
- 10-second law: every sweep and profile in the background to a file. Any
  single bench cell over 10 s is reported as a defect line, not hidden.
- Never seize the machine: one bench process at a time, `nice -n 10`.
- Vocabulary: products and rows. Banned words: provenance, substrate,
  load-bearing, regime, ground truth. No em dashes.

## Stop

Stop and report when: the three worst `ivm/dd` cells are each under 3x, or
a change needs a schema format bump, or a circuit needs a new operator in
the engine. Those are the parent's calls.

## Report

`boop beep --no-wait --as perf/toward-dd sprefa-coordinator "<one line>"` at
each PR: PR number, cell, before/after numbers, `ivm/dd` before/after.
