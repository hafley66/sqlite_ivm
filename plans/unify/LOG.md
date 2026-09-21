# Overnight herd log, 2026-09-20

Target (user, 2026-09-21): sqlite_ivm provably as close to DD as the plugin
can get, on map, filter, join, group, window, distinct, set ops, recursion.
Every bench reports per arm: wall, peak RSS, bytes read and written to disk,
RAM. The yardstick column is `sqlite-ivm / dd` per circuit per n. Not every
SQLite shape, all the juice. Then: sprefa v8 compiler on sqlite_ivm instead of
DD; sqlite_ivm under ryi / sprefa-extract; dl8 generates parts of ryi.
Every row below is a receipt or a defect.

Rungs: dispatched, running, reported, verified (named check), merged.

## Board

| lane | preset | job | rung | pr |
|---|---|---|---|---|
| refactor/bench-rust | glm53f-omp-max | arc A: Rust bench binary, shootout + scale | running | |
| fix/open-bugs | sol-med | 6 items: 14_scale ignore, window LIMIT, growth assert, subtype ledger, TEXT keys, hygiene | running | |
| chore/archive-labs | sonnet subagent | labs, probes, crud scripts to archive/; lab issues closed | queued | |
| refactor/bench-rust arc B | glm53f-omp | delete 55 bench files | queued on arc A | |
| perf/toward-dd | flash-omp-max | ivm/dd yardstick per circuit, peak RSS, disk io; profile-driven arcs, one circuit each; brief `perf-dd.brief.md` | queued on bench arc A + bugs | |
| refactor pass 1..4 | glm53f-omp | see passes | queued on bugs | |

## Passes (module map 2026-09-21, `src/` 5660 lines, no dead pub items, no >15-line duplicate blocks)

Serial, each from the main that holds the one before. Behavior-preserving
unless the row says otherwise. Executor glm53f unless noted.

| pass | what | why (fact from the map) | receipt |
|---|---|---|---|
| 1 | split `src/1a_relational.rs` `impl Plan` (1180 lines) into ddl / materialize / drain files; one fn per `Kind` arm | `materialize` match is 312 lines, Join arm 112, Group 92 | pure move: battery identical, `13_statements_per_drain` counts identical, no `format!` string changes (`git diff -w` on moved text) |
| 2 | per-view statement program: every drain-path SQL string built once at connect into a struct keyed by node; drain runs prepared statements only | 187 `format!` sites in `1a`, 11% of profile is SQL text rebuild, 20% statement-cache hashing | `time.busy` on `8_group_limit`, `13_statements_per_drain`, three runs each side; counts unchanged; glm53f-omp-max |
| 3 | retire the single-row trigger path (`0_query.rs` 372 + `1_maintenance.rs` 411) if the relational plan answers every shape in `tests/0_query.rs` and `tests/1_maintenance.rs` | two engines behind one vtab (`2_vtab.rs` imports both `maintenance` and `relational_maintenance`) | those tests pass through the relational path, or a table of shapes that do not, and the pass stops there |
| 4 | split `src/0b_relational.rs` `impl Compiler` (1170 lines) by clause; `Field` literal helper (4 copies at 791, 1309, 1337, 1403); `2_vtab.rs` `dispatch` and `migrate` read | largest file, one impl | pure move, battery identical |

## Entries

- 03:5x merged #20, #21, #22; main `30a6e9c`.
- primary checkout `~/projects/sqlite_ivm` is stale at `39c6ac2` with older
  untracked copies of issue and docs files; the classifier blocked moving
  them. Lanes run from `origin/main`; nothing depends on it. Morning: move
  the strays aside, `git pull --ff-only`.
- dispatched refactor/bench-rust (m-eecc57e1), fix/open-bugs (m-9ba06a98).
- 04:2x chore/archive-labs PR #23 graded (66 files, owned paths only, `0_query` green) and merged; main `fd4dadd`. Six lab issues closed.
- fix/open-bugs commits so far: `44ecad9` 14_scale ignored, `553ca21` window LIMIT copies CTE (reviewed: ordinal `copies` cross join, budget check, one extra `max(__n)` statement per group materialize; rail 13 decides), `b5c6c37` group limit growth pin.
- 04:2x fix/open-bugs item 5: three interning commits `3cee12b` (_state keys), `c86d76f` (delta scratch), `69ec8cc` (fixpoint deletion keys), each measured slower on all three runs: `8_group_limit` time.busy 6.6 -> 7.4 -> 8.6 -> 10.5 s. Lane wrote the numbers itself. Hails m-73c94534, m-5613d2d0 order the reverts; undelivered until the codex turn ends. Finding for Chris: "intern the composite" (2026-09-20) holds on arrangement tables (landed earlier) and loses on the per-drain state and scratch sites; TEXT stays there.
- 04:4x checkpoint. fix/open-bugs opened PR #24 (76 pass x3) with the three interning regressions still in; graded red, hail m-a7c03432 orders the reverts pushed to the PR. refactor/bench-rust 25 min in, `bench/src/` and `plans/costs/shootout-rust.md` on disk, no commit yet.
- 04:5x fix/open-bugs PR #24 graded: src differs from main by the 16-line window fix only; post-revert `8_group_limit` 6.40, 6.86, 8.08 s against 6.54-6.84 before (same code, noise); `8_group_limit`, `13_statements_per_drain`, `7_key_agreement` green here; owned paths only. Merged; main `f993c14`. Closed: window-limit, group-cte, json-subtype, json-text-keys (verdict recorded), dylib.
- dispatched refactor/pass-1-split-relational-maintenance (m-9351db0e, glm53f-omp) from `f993c14`. Two build lanes live: bench-rust, pass-1.
- 05:1x checkpoint. bench-rust: 2775 lines in `bench/src/` (fixture, oracle, report, five arms), zero commits at 44 min; hail m-23283028 orders scoped commits now. pass-1: commit 1 of 4 (`dc63e8d`, `1b_state.rs`). No PRs open.
- 05:3x refactor pass 1 PR #25 graded: 75/75 x3 here, clippy clean, `format!` strings byte-identical, only owned files, `1a` 259 / `1b_state` 327 / `1c_materialize` 406 / `1d_drain` 553 lines, one fn per arm. Merged; main `a35146c`.
- dispatched refactor/pass-2-statement-program (glm53f-omp-max) from `a35146c`; brief `refactor-2.brief.md`.
- 05:5x checkpoint. bench-rust at tool call 610, its own words "all cells pass", fixing pg_ivm error text and disk columns; still zero commits (omp takes the commit hail at turn end). pass-2 running 10 min, no commit. No PRs open.
- 06:1x checkpoint. bench-rust 84 min, tool call 861, "remaining items (4)", zero commits. pass-2 30 min, `1e_program.rs` on disk, editing after a stale-offset mishap, zero commits. Both alive. No PRs.
