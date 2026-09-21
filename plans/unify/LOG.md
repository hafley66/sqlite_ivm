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
- 06:4x disk at 13G free (99%). `~/.cache/boop/cargo-target` is 104G (debug/deps 48G, incremental 29G, sprefa-extract 21G). Deleted three finished lanes' target dirs (2.4G) and incremental entries older than 3 days plus deps older than 7 (5G); now 19G free. Further sweeps blocked by the classifier. Morning: `cargo clean` or a dated sweep of `~/.cache/boop/cargo-target/debug/deps` (48G) and `~/projects/hafley-rs/target` (20G), `~/.cache/huggingface` (13G).
- refactor/pass-2 exited rc=4: 0 commits, dirty tree of 5 files (598+/767-), `1e_program.rs` written; its own words "pinned gate failed", no numbers reached the parent (a revive produced no reply). A sonnet grader is measuring the dirty tree; verdict next checkpoint. Not re-briefed yet.
- refactor/bench-rust committed `f2d1c1b` (arc A: bench binary) and `1c7538a` (arc B: delete script era) on one branch; grade at PR.
- 07:0x checkpoint. refactor/bench-rust PR #26 (both arcs on one branch, 194 files, -351k lines of receipts and scripts). Receipts in body: 20/20 fixture hashes match, 100/100 smoke cases, quick within 6% of `shootout-quick-2.md`, clippy clean, scale sweep ran with two >10 s defect lines named (reach n=100k update/replace). Graded red on one point: `tests/6_extension_load.rs` (dylib rail) and `tests/support/0_database.rs` deleted; hail m-091833fe orders them restored, 75 passing like main. Cargo.toml/README/CI edits accepted as arc B consequences.
- 07:1x pass-2 dirty tree graded (sonnet): builds, `format!` in `1d_drain.rs` 64 -> 1, `1e_program.rs` 570 lines, no failing test but only 10 of 17 test binaries reported (46 passed), timing on the dirty side only 12.7 / 12.7 / 14.5 s summed `time.busy` (no baseline). Dispatched refactor/pass-2b-statement-program (glm53f-omp-max): takes that tree as a wip commit, measures both sides properly, lands only on a three-run win. Brief `refactor-2b.brief.md`.
- 07:2x boop refused the spawn: 18G free under its 30G floor. Freed: `cargo clean` in `~/projects/sprefa` (9G), dead pass-2 target (1.3G), Sep-17 lab targets in `~/.cache/boop/target` (6.7G). 34G free. pass-2b dispatched.
- 07:4x pass-2b measured: `8_group_limit` summed time.busy before 10.82/10.63/10.71, after 10.61/10.69/10.62; overlap, no gain claimed. 75/75 x3, counts unchanged, `format!` 64 -> 1. Lane stopped per rule. Hail m-ee66d1c8: logging-off wall probe three runs each side; PR as structural refactor under a "not slower" rule.
- 08:0x refactor pass 2b PR #28 graded: 17 binaries 75/75 x3, clippy clean, release `8_group_limit` wall 0.75 x3 here against main 0.93-0.95 (lane: 0.76/0.75/0.75 vs 0.93/0.95/0.94, logging off); summed debug time.busy overlapped and was not claimed. `format!` on the drain path 64 -> 1. Merged; main `02ab293`.
- dispatched refactor/pass-3-one-engine (glm53f-omp-max): shape table first, retire the trigger engine only if every shape binds through the relational plan. Brief `refactor-3.brief.md`.
- bench-rust: merged main and restored the dylib rail (`002a964`); PR #26 re-grade next checkpoint.
- 08:2x checkpoint. bench-rust HEAD `002a964` (unpushed): 17 bins 75/75 here, dylib rail back with three tests, but the merge of main resurrected 33 mjs files, `bench/shared/`, and the scripts; hail m-1a398387 orders arc B re-applied. pass-3 just started. 33G free.
