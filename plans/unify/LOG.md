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
- 08:5x bench PR #26 graded: 16 binaries 75/75 x2 (14_scale deleted by plan), dylib rail restored with three tests, script era gone (0 mjs/py/pl on main), five sh scripts remain (build, package, statement-costs, crud and verify kept by #23 for references). Merged; main `00f37b2`. Bench: `bench/` crate `sqlite-ivm-bench`, subcommands `shootout`, `scale`, `dump-fixture`; receipts in `plans/costs/shootout-rust.md`.
- pass-3 step 1 done (`9aebf80`): every old-path shape binds through the relational plan; proceeding to retire.
- dispatched perf/toward-dd (flash-omp-max) from `00f37b2`.
- 09:1x checkpoint. Stray PR #27 (arc B stacked on the old #26 head) closed as superseded by #26. pass-3 retiring (18 min, no second commit yet). perf/toward-dd 8 min in. 34G free.
- 09:3x checkpoint. pass-3 mid-retire: `0_query.rs` and `1_maintenance.rs` deleted in the tree, catalog/relational edited, not committed. perf/toward-dd adding a `scale_dd.rs` arm to the bench (the `scale` subcommand lacked a dd arm). Both alive. 33G free.
- 09:5x checkpoint. pass-3: 18 files dirty (both old engine files and `tests/0_query.rs` deleted, tests being moved), one build error outstanding, no commit since the shape table; hailed to commit at first clean build. perf/toward-dd: 7 dirty bench files, no commit. 33G free.
- 10:0x pass-3 exited twice with the retire uncommitted (18 files, one build error); a revive delivered and retired again. Dispatched refactor/pass-3b-one-engine (glm53f-omp-max) to take the tree, commit first, then receipts. Brief `refactor-3b.brief.md`. Pattern noted: glm lanes exit at a gate without committing; every future brief opens with "commit before measuring".
- 10:0x z.ai plan quota exhausted ("429 Usage limit reached for 5 hour", resets 16:57). That is why pass-3 exited silently mid-turn, twice. pass-3b respawned on `flash-omp-max` (deepseek). glm presets are off the table until 16:57; pass 4 goes to deepseek too.
- 10:3x perf/toward-dd arc 1 PR #29 graded: bench-only, clippy clean, 36-cell sweep verified against recompute, clean tree. Merged; main now carries the baseline. Numbers (`plans/costs/toward-dd/README.md`): every cell above 3x vs DD; worst `reach` fanout 1 n 100000 at 82207x (ivm 644 s, dd 7.8 ms), `reach` fanout 10 n 100000 at 692x, `chain` fanout 1 n 100000 at 531x; best `group` fanout 1 n 100000 at 8.6x. `chain` fanout 10 n 10000 already 12.9 s per cell. Recursion is the fire; the lane's arc 2 takes the worst cell by brief.
- pass-3b: retire committed (`ddde334`), receipts running.
- 10:5x checkpoint. pass-3b on receipts (37 min since the wip commit, 2 dirty files). perf/toward-dd arc 2 started: editing `bench/src/scale.rs` (probably a single-cell mode for the profile). No PRs. 33G free.
- 11:1x checkpoint. pass-3b amended its wip (`54fcdfa`), probing `2_vtab.rs` with a temp test; 57 min on receipts. perf lane 20 min since its last commit, profiling. No PRs. 33G free.
- 11:4x refactor pass 3b PR #30 graded: 15 binaries 68/68 x3 (75 - 8 internals - 1 duplicate + 2 moved), clippy clean, `13_statements_per_drain` counts identical, `src/` 6173 -> 5348 lines, `8_group_limit` and `4_features` release wall not slower. One engine behind the vtab: `0_query.rs` and `1_maintenance.rs` gone. Merged; main `bdfe84e`.
- dispatched refactor/pass-4-compiler-split (flash-omp-max) from `bdfe84e`; brief `refactor-4.brief.md`.
- 12:0x checkpoint. perf lane 60 min since last commit, no src change yet; hailed for a three-line status. pass-4 18 min in, no commit. No PRs. 34G free.
- 12:4x checkpoint. perf lane arc 2 finding (`c30ea36`, `plans/costs/toward-dd/cliff.tsv`): `reach` fanout 1 closure is one hop deep, yet per-statement cost cliffs 29x between n 40000 and 60000; 36% of the n 100000 cell is ephemeral-table setup and teardown. A cache-size or temp-store cliff, not an algorithm. Engine change pending. pass-4: five commits, four extractions redone once, receipts running. 33G free.
- 13:0x refactor pass 4 PR #31 graded: 68/68 x3, clippy clean, `format!` strings byte-identical, `0b` 696 / `0c_compile_from` 555 / `0d_compile_select` 589 / `0e_compile_recursive` 315 / `0f_columns` 65 lines, one `Field` constructor, release `8_group_limit` 0.78-0.80 s. Merged; main `186d051`. All four refactor passes landed.
- dispatched docs/failure-ledger-rows (flash-omp, docs only): the six failure-modes rows owed since the 2026-09-19 session. Brief `ledger.brief.md`.
- 13:2x checkpoint. perf lane clean tree, no new commit in 40 min (profiling the cliff). ledger lane 9 min in. No PRs. 33G free.
- 13:4x docs PR #32 graded (one file, six rows, two cells `not found` with the search named) and merged; main `a4756c9`. Board: only perf/toward-dd remains live.
- 14:0x checkpoint. perf lane first engine edit in the tree (`1d_drain.rs`, `1e_program.rs`, 13+/11-): the fixpoint `restore` statement becomes a list, the ephemeral-table cliff fix in progress; uncommitted. Only lane live. 33G free.
- 14:2x checkpoint. perf lane same two files dirty for 40 min, measuring; hailed to commit wip. 34G free.
- 14:4x checkpoint. perf lane in one long turn since 12:00 (two hails held). `1d_drain.rs` edit reverted, only `1e_program.rs` dirty: it tried the restore change, measured, and backed it out, as the rule says. 4.1 h alive, arc 2 unlanded. 33G free.
- 15:0x checkpoint. perf lane hypothesis (its comment in `1e_program.rs`): the fixpoint rederivability test was one `WHERE EXISTS(..) OR EXISTS(..)` per work row, which SQLite cannot flatten, so each subquery re-runs per row; rewritten as `UNION ALL` branches, one statement, counts unchanged. Measuring. 33G free.
- 15:2x checkpoint. perf lane arc 2 attempt 1 rejected by its own numbers: `UNION ALL` branches made `reach` fanout 1 n 100000 3.3x slower (2141 s vs 644-648 s), killed at rep 0, row in the ledger draft. Per-statement cost cliff 351 ms at n 40000 vs 10190 ms at n 60000 stands as the finding. Lane on attempt 2. 32G free.
- 15:4x checkpoint. perf lane: src clean again (attempt 1 reverted), ledger draft and a bench Cargo edit dirty, 5.1 h alive, one long turn, two hails still held. No PR. 32G free.
- 16:0x perf lane `ae4f50c`: symbolized profile corrects the earlier reading. `reach` fanout 1 n 100000 is 91.9% SQLite row scan and record deserialization, not ephemeral-table setup (that was nearest-symbol attribution). Two SQL changes rejected with numbers (UNION ALL restore 3.3x slower; range-driven drop_deleted_range no change). No src commit. So the fixpoint's departure pass scans a big table per round; the fix is an index or a smaller work set, not statement shape.
- 16:2x perf lane `d43d8c5`, the root of the `reach` cliff: delete (departure) rounds grow 5795 at n 10000 to 52236 at n 40000 while derive rounds stay flat at ~165. Each delete round scans the closure, so cost is rounds x closure = superlinear. The departure fixpoint iterates one row per round instead of set-at-a-time; the derive side already got set-at-a-time in PR #21. That is the fix shape.
- 16:4x perf PR #33 graded (bench + ledger only, no src; a failure-modes row for the mis-attributed profile) and merged; main `1f64504`. Lane is on the engine fix in `1e_program.rs` (departure set-at-a-time). 32G free.
- 17:0x checkpoint. perf lane idle at a turn boundary after "Delivered" (arc 2). Sent arc 3 go: departure fixpoint set-at-a-time, mirror of #21's derive side, reach cells three runs each side. 32G free.
- 17:1x perf lane `f337963`: departure rounds are not monotonic in n (5795 at 10000, 343027 at 16000, 572824 at 32000, 52236 at 40000). Round count depends on the seed's graph shape (`(id*7)%(n/10+1)` keys), not n. Set-at-a-time departure still removes the rounds term whatever the shape; arc 3 stands. Third rejected change logged (correlated EXISTS in derive, no change).
- 17:3x PR #34 (ledger only) merged; main `2e3ae53`. Arc 3 go delivered 12:12; lane on the departure rewrite. 32G free.
- 17:5x checkpoint. perf lane clean tree, no src edit yet on arc 3 (reading `1d_drain.rs` fixpoint). 6.5 h alive. 32G free.
- 18:1x checkpoint. perf lane: baseline banked (`5c73317`), src clean, rewrite not yet in the tree. 32G free.
- 18:2x perf lane `4b6171b`, the bill: the departure derive step joins the work rowid slice to the input relation one hop per round, and both sides of the join predicate are wrapped in a cast, so no index can serve it (full scan per round). Two fixes stack: set-at-a-time (rounds) and an index-serving predicate (scan). Lane proceeds.
- 18:4x checkpoint. PR #35 (ledger only) merged. Disk fell to 22G: 9 GB of `/private/tmp/item5-*.log` from the bugs lane's HAFLEY_LOG runs (647 MiB each, 130 log files in /tmp). Deleted tonight's; 31G free. Rail for future briefs: measurement logs go under the lane's `CARGO_TARGET_DIR`, never /tmp.
