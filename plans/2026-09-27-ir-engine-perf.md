# IR engine compile measurement, 2026-09-27

## Repro and timing

Repro: `sprefa/.boop-worktrees/chore/perf-repro`, whose `sqlite_ivm` symlink targets this worktree. Release `dl8 compile --trace fixtures/comptime_effect/0_fs_json.dl7`, with `DL8_ENGINE` set as shown. `CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/perf-repro/target`, `CARGO_BUILD_JOBS=4`, `CARGO_INCREMENTAL=0`, and `cargo build -j 4 --release --bin dl8`.

| Engine | Three wall times, seconds | Median | Target |
| --- | --- | ---: | ---: |
| rust | 0.164, 0.141, 0.183 | 0.164 | baseline |
| ir:dd | 3.036, 2.788, 2.665 | 2.788 | <= 0.328 (2x measured rust median) |
| ir:sqlite | 12.259, 12.160, 12.095 | 12.160 | <= 0.820 (5x measured rust median) |

The issue's fixed targets use its 0.155 s rust baseline: DD <= 0.31 s and SQLite <= 0.78 s. The `--trace` phase lines in one run reported read 12 ms, macrotime 985 ms, compiler lower 3 ms, check 2 ms, comptime 1597 ms for DD. SQLite reported 12, 4496, 3, 2, 7518 ms. These phase lines do not time the IR lowering inside each evaluation.

Temporary engine entry/exit probes, removed before this commit, measured the following on a single instrumented compile. Wall totals include process startup and sprefa work; engine timings are sums of wall durations in `Engine` calls. These are sequential on the SQLite main thread; DD's worker runs behind the caller.

| Engine | Wall ms | Engine install ms (5 calls) | Engine settle ms (31 calls) | Engine snapshot ms (603 calls) | Remaining ms, including sprefa IR lowering, decode, driver, and startup |
| --- | ---: | ---: | ---: | ---: | ---: |
| ir:dd | 2661 | 810.0 | 1516.7 | 3.2 | 331.1 |
| ir:sqlite | 12270 | 5602.6 | 6126.3 | 305.9 | 235.2 |

The remaining column is subtraction, not an independent stopwatch. Samply sampled `dl8::_6_eval::ir_lower::lower` in 22 of 2785 DD thread samples and 24 of 13343 SQLite samples; `snapshot_closure` appeared in 11 and 134 samples. The profile is CPU sampling at 1000 Hz, so the DD total includes worker threads and cannot be treated as wall time. Decode is included in the remaining column. Engine snapshots exclude `intern_snapshot` and text lookups.

## IR size and frontiers

Each `Engine::install` receives a fresh IR program. The first three evaluations occur in macrotime; the last two are comptime rounds 1 and 2. The numbers below came from temporary probes at the engine boundary, removed before commit. Frontier lists give the number of `SourceChange` entries in order of `Engine::settle` calls. Each row applies to both IR engines.

| Evaluation | IR nodes | Strata | Outputs | Relations | Frontier sizes |
| --- | ---: | ---: | ---: | ---: | --- |
| macrotime round 1 | 1615 | 29 | 44 | 106 | 156, 163, 56, 26, 953 |
| macrotime wave 1 | 1619 | 30 | 45 | 109 | 212, 213, 93, 37, 963 |
| macrotime wave 2 | 1601 | 26 | 41 | 99 | 286, 298, 110, 24, 1268 |
| comptime round 1 | 4285 | 183 | 104 | 428 | 483, 515, 134, 102, 3097, 5, 5, 3, 1, 3 |
| comptime round 2 | 4285 | 183 | 104 | 428 | 486, 521, 138, 107, 3, 3128 |

The two comptime programs have identical per-stratum reachable IR node counts. Counting each distinct node reachable from a stratum's body or bodies, including `Get` and nodes shared by other strata, gives 183 counts, sum 4638, median 12, maximum 826. Counts in stratum order:

```text
2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,3,12,3,3,3,3,3,3,3,3,7,5,5,7,7,5,5,5,15,5,5,7,5,5,5,5,5,7,5,5,5,5,7,7,7,28,15,10,9,28,26,26,26,28,28,28,28,28,28,28,55,30,30,31,15,16,11,9,11,19,19,7,19,19,11,13,33,30,19,16,7,7,9,7,9,9,19,23,9,75,7,19,16,21,12,16,24,11,64,31,31,37,37,37,13,14,41,41,41,41,41,35,41,41,41,43,4,826,15,35,77,41,41,9,7,9,45,45,45,81,4,27,10,13,7,4,7,11,16,7,19,9,10,28,11,11,29,24,27,42,18,19,21,25,25,11,11,16,13,7,16,19,7,19,131,9,9,11,102,4,12,8,27,46,47,27,413
```

For the macrotime programs the corresponding reachable-node counts have sums 1788, 1792, and 1774, and maxima 1308 in each. These are IR graph counts, not Timely's internal operator count.

The traced comptime results were: round 1, 129 rules, 1564 seeds, 1575 closure rows, one effect and three answer rows; round 2, 129 rules, 1570 seeds, 1577 closure rows, stable. Both engines emitted the same round counts. DD's per-round install/settle/snapshot milliseconds were 168.5/669.0/1.5 and 168.3/451.4/1.0. SQLite's were 2043.7/1775.7/167.3 and 2112.3/1403.3/113.4. The 104 visible outputs account for repeated snapshot calls during term bootstrap and closure decode.

## Samply top frames

`samply record --save-only` profiles were captured for each IR engine. The Rust symbols below were resolved from the release binary's symbol table. Counts are sampled leaf frames, across all threads, with compiler-generated type suffixes shortened. Unknown system-library offsets are omitted from this named-frame list. DD: 2785 samples; SQLite: 13343 samples.

| Rank | ir:dd leaf frame (samples) | ir:sqlite leaf frame (samples) |
| ---: | --- | --- |
| 1 | `timely::progress::reachability::Builder::build` (411) | `sqlite3VdbeExec` (2231) |
| 2 | `BinaryHeap::pop`, product timestamp (139) | `sqlite3FkClearTriggerCache` (585) |
| 3 | `BinaryHeap::pop`, outer timestamp (137) | `core::io::error::os_functions::set_functions_inner` (325) |
| 4 | `Subgraph::propagate_pointstamps`, product timestamp (128) | `sqlite3FindTable` (266) |
| 5 | `quicksort`, Timely reachability runs (87) | `sqlite3Malloc` (262) |
| 6 | `Subgraph::propagate_pointstamps`, outer timestamp (87) | `sqlite3RunParser` (218) |
| 7 | `BTreeMap::entry`, DD tap rows (82) | `yy_reduce` (216) |
| 8 | `smallsort`, Timely reachability runs (75) | `btreeParseCellPtr` (194) |
| 9 | `quicksort`, Timely outer change batch (75) | `sqlite3BtreeNext` (189) |
| 10 | `quicksort`, Timely product change batch (68) | `getCellInfo` (183) |

Inclusive stack counts locate the installation work: DD `Worker::dataflow_core` 828 samples, `ivm_engine::rel::lower` 763, `Nest::letrec` 733, and Timely reachability builder 684. SQLite `Engine::install` 6281 samples, `catalog::install_program` 6257, and `sqlite3Prepare` 5616; `Engine::settle` 6506 and `engine::settle_inner` 6354. DD's `Worker::step_or_park` appeared in 1484 samples. Inclusive counts overlap and are not percentages of wall time.

## Stop condition and required sprefa change

The repeated engine lifecycle is owned by sprefa. `src/_4_comptime/_2_rounds.rs::round_closure` calls `evaluate_round_answered` on each round, and `src/_6_eval/_9b_ir_eval.rs::evaluate_ir_with_answerer` lowers the complete program then `run` installs a new engine, bootstraps the term dictionary, replays all seeds, and decodes all visible outputs. Comptime round 2 repeats the 4285-node program installation and a 3128-row seed frontier after round 1 had already settled a 3097-row frontier. SQLite opens a new in-memory connection for each evaluation. The repeat occurs outside `ivm-*`, so engine statement caches and worker state cannot survive it.

The change belongs in sprefa: keep an IR evaluation session in `RoundState` across inner comptime rounds. Separate the stable rule/stratum program from source seed rows, compare structural IR between rounds, retain the engine, host connection, text and term mappings when the structure matches, and apply a frontier containing seed removals and additions. Preserve Once answerer state and decode closure from the retained engine. Reinstall only when generated rules or relation shape changes, or at the outer refreeze boundary. The IR graph of the two measured comptime rounds has identical node, stratum, output, and per-stratum reachable-node counts; structural equality still needs checking before reuse. Macrotime's three programs have different shapes and require separate handling.

This was the user-specified stop condition for the first measurement pass. No permanent engine source change was made in that pass. An isolated `ivm-*` edit cannot remove the repeated lowering, install, bootstrap, and replay from this compile path.

## Validation at stop

`SEEDS=1000 CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 cargo test -j 4 -p ivm-ir -p ivm-engine -p ivm-dd -p ivm-sqlite -p ivm-rxjs` ran through the random tests, then failed at `ivm-sqlite --test extension`: its nested offline build could not read `/tmp/ivm-sqlite-extension-target/frontier-ext/debug/build/libsqlite3-sys-304cc306755aa11e/out/bindgen.rs` (the actual macOS temporary root was under `/var/folders`). A direct retry of that test produced the same missing-file error. The extension test uses a fixed temporary target directory. No source file was modified to address this independent test-build failure.

`CARGO_TARGET_DIR=$HOME/.cache/boop/lanes/perf-repro/target CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 cargo test -j 4 --test it` in the read-only sprefa repro worktree finished **85 passed, 56 failed, 141 total**. Failures include the `sqlite` evaluator returning the existing `not_built_yet("eval open")` diagnostic in the ladder tests and 0/32 SQLite evaluator oracles. The requested 141/141 gate was not met. No sprefa source was edited. There is no after-performance timing because the stop condition precluded an engine change.

## DD continuation: recursive-scope inputs

The later engine-only task measured 168 `Let` and 15 `LetRec` strata in each 183-stratum comptime program. `Let` was already in the outer scope. Before the change, each `LetRec` entered every source and every preceding defined collection into a fresh iterative scope, including those not read by its bodies. The 21-relation recursive scope entered 222 sources and 125 preceding relations. A traversal of its IR bodies found 66 referenced relation IDs; only matching collections now enter that scope. A `LetRec` body with no reference to any of its own IDs lowers in the outer scope.

Temporary scope timers measured the 21-relation scope at about 134 ms before and 118 to 129 ms after in comptime; the 18-relation macrotime scope measured about 167 ms before and 144 ms after. Samply at 1000 Hz recorded 411 sampled leaf frames in Timely reachability `Builder::build` before and 370 after. These CPU sample counts are an estimate of work in that frame, not precise wall durations.

After a release rebuild, three `--trace` timings were rust 0.138/0.117/0.137 s (median 0.137), ir:dd 2.240/2.238/2.408 s (median 2.240), ir:sqlite 12.277/12.065/12.037 s (median 12.065). The initial DD median was 2.788 s. `SEEDS=1000 cargo test -j 4 -p ivm-ir -p ivm-engine -p ivm-dd` passed with `CARGO_BUILD_JOBS=4`, `CARGO_INCREMENTAL=0`, and `RUST_TEST_THREADS=4`.

## SQLite continuation: install SQL

The repro connection reported `PRAGMA foreign_keys=1`. A source search of `ivm-sqlite` and `ivm-ir` found zero `FOREIGN KEY` or `REFERENCES` clauses in engine DDL. Temporarily switching foreign keys off during install did not reduce cost: the five installs' DDL time was 4674 ms before and 4970 ms with the switch, so that experiment was removed. The DDL is already issued as one `execute_batch` per installed program, but that batch contains 26,108 `CREATE` statements across the five evaluations. SQLite prepares and applies each schema statement separately.

The retained SQLite changes are in the SQL node lowerer and bootstrap. An `Mfp` with no filter or map and an identity projection returns its input node. A one-input `Union` also returns its input. This avoids tables for pass-through nodes. For typed IR, empty source tables cannot produce rows, so bootstrap checks sources with batched `EXISTS` queries and defers preparing the fill statements until the first settle. The check retains the existing bootstrap path when any source has rows. A per-source emptiness check measured a 11.559 s median SQLite compile; removing it measured 11.730 s; batching the checks measured 11.161 s. Each was three release `--trace` runs of the same fixture. These are wall timings under concurrent host load, and the remaining schema creation cost dominates the measured install.

With the final source, a later three-run set measured rust 0.165/0.130/0.129 s (median 0.130), ir:dd 2.521/2.586/2.604 s (median 2.586), and ir:sqlite 12.525/12.752/12.030 s (median 12.525). Another lane had CPU-heavy `dl8` and Rust compilation processes active during this set. The paired SQLite A/B results above isolate the effect of the retained changes more closely. Neither engine reached the issue's fixed release target.

`SEEDS=1000 cargo test -j 4 -p ivm-ir -p ivm-engine -p ivm-dd -p ivm-sqlite -p ivm-rxjs` passed on the final SQLite code. The nested frontier extension test first failed due a stale `libsqlite3-sys` build output in its fixed temporary target; cleaning only that package and rebuilding made it pass. `bash scripts/0_build.sh release` produced the extension loaded by sprefa. With an explicit `LC_ALL=en_US.UTF-8`, sprefa `cargo test -j 4 --test it` reached 140/141: the only failure compares a pinned book excerpt for `dl8 run --help` that omits `fs.watch` against current CLI output. All engine oracle and ladder tests passed. On the final batched-bootstrap source, the targeted `ir_engines_match_v7_oracles` test passed. No sprefa source or oracle expected output was edited.
