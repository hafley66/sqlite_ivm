# ivm-duckdb engine experiment, 2026-10-05

## Transport candidates

| Transport | Rust process boundary | Input / output | Runtime requirements |
| --- | --- | --- | --- |
| DuckDB CLI child | DuckDB runs in a separate process; no DuckDB linking | SQL stdin, JSON stdout | Matching CLI and OpenIVM extension |
| ADBC driver manager | Runtime-loaded DuckDB driver executes inside the Rust process | SQL statements, Arrow arrays | Driver manager, DuckDB shared driver, Arrow FFI |
| ODBC driver manager | Runtime-loaded DuckDB driver executes inside the Rust process | SQL statements, bound result columns | Driver manager, DuckDB ODBC driver, DSN or connection string |

Selected: persistent CLI child, plain SQL and JSON. This preserves the external-process requirement. ADBC and ODBC load the engine into the caller process.

Sources: [CLI](https://duckdb.org/docs/current/clients/cli/overview), [ADBC](https://duckdb.org/2023/08/04/adbc), [ODBC](https://duckdb.org/docs/current/clients/odbc/overview).

## Versions and build

OpenIVM: `cec35ae41e930a607db774673310e6cc263775bd` in `~/projects/openivm`. DuckDB: v1.5.2 (Variegata), commit `8a5851971fae891f292c2714d86046ee018e9737`. The matching CLI and loadable extension built successfully with `cmake --build build/release --target duckdb openivm_loadable_extension -j 4`. Build configuration: Release, Ninja, unit tests disabled, unity builds disabled. Required DuckDB, extension-ci-tools, LPTS and LPTS DuckLake submodules were initialized. OpenIVM working tree remains clean. No DuckDB Cargo dependency.

## Implementation and verification

Confirmation: 26 passed / 8 failed across 34 shared cases. Two passing cases contain no captured frontiers and exercise install only. The separate string lifecycle + NUL + mark/rewind test passed. Cargo integration-test result: 1 passed / 1 failed, 0 ignored. Status: blocked on conformance.

## Execution contract

Set `IVM_DUCKDB_CLI` to the pinned build's `build/release/duckdb` and
`IVM_DUCKDB_EXTENSION` to `build/release/extension/openivm/openivm.duckdb_extension`.
A fresh temporary database and one persistent CLI child belong to each installed engine.
The process receives SQL and emits framed JSON. No install image or result cache is used.

Sources are sets. Intermediate tables carry signed `BIGINT` weights. Output deltas
are the grouped SQL difference between the new output and its previous table.
Dictionary IDs, text, constructor rows, Any payloads, and structural ordering keys
are persisted in SQL tables. Mint and string scalar boundaries use the shared Rust
IR semantics and write their recomputed rows to DuckDB. They log their recompute path.

OpenIVM creates and refreshes views through separate internal connections
(`src/core/parser.cpp`, `src/upsert/refresh.cpp` in its checkout). Thus a caller
transaction cannot enclose source writes and `PRAGMA refresh`. The CLI engine
checkpoints its private database before a settle. A failed settle terminates the
child, restores the checkpoint, and reopens the database. Mark/rewind use another
checkpoint, including OpenIVM auxiliary tables. Database copies are rollback state;
no warm install is reused.

Nonrecursive SQL nodes use OpenIVM materialized views, except nodes that read dictionary tables. Get, Negate and Threshold request incremental refresh. Mfp, Union, Join, Reduce, TopK and Window request full refresh for their signed-weight consolidation query. The first run of the adapter with the pinned OpenIVM revision lost retractions in those grouped queries; the cleanup made this fallback explicit. This experiment does not isolate an upstream OpenIVM defect. Dictionary-reading nodes recompute SQL tables outside OpenIVM tracking, since the extension failed to serialize NUL text while tracking its dictionary inserts. The engine logs these choices and refresh strategy IDs. Recursive
programs currently recompute their node tables in SQL, with simultaneous LetRec
rounds from empty relations and nested scopes evaluated inside each outer round.
A limit violation restores the entire prior database. DuckDB supports recursive
CTEs; this OpenIVM revision lists them as full-refresh only. The explicit round
loop also accommodates the IR's mutual, nested, and nonmonotone recursion.

Current explicit unsupported surface: Delay, Real/Any arithmetic and same-rank scalar comparison, Real/Any join and antijoin keys, and Real/Any reduce or ordered-window inputs. Pure projection can carry these cells unchanged. Dictionary intern/read methods support Any payloads. This gap
must not be described as full typed Engine conformance.

`Work.statements` counts SQL statements sent by the Rust adapter, including its
checkpoint statement. OpenIVM's internal SQL statements are outside that count.
`rows_in` counts accepted source changes; `rows_out` counts JSON rows received,
including metadata and dictionary reads; `refreshes` counts successful explicit
refresh calls. `Counters.rows_written` counts the nonzero net output delta rows.
`Work` records the latest attempted settle, including restore work on a rejected frontier. Trait counters retain the most recent successful settle.

## Conformance receipt

Command: `IVM_DUCKDB_CLI=~/projects/openivm/build/release/duckdb IVM_DUCKDB_EXTENSION=~/projects/openivm/build/release/extension/openivm/openivm.duckdb_extension cargo test -p ivm-duckdb -j 4 -- --nocapture --test-threads=1`. The shell run had an outer timeout; neither run reached it.

The test sequence was one initial execution, two code cleanup passes, and one confirmation execution. No further implementation edits or test reruns followed confirmation.

- Pass 1: removed unchecked assumptions about incremental grouped refresh; made grouped and dictionary fallbacks explicit; removed raw-integer Real/Any ordering; allowed pure typed projection; used actual DD install behavior instead of a stale expected-error header.
- Pass 2: restored checkpoints when the CLI already exited; preserved the original error if restore also fails; included post-settle dictionary reads in rollback handling; added explicit mixed-type comparison and scalar arity rules; rejected unsupported typed join keys; checked projection columns; rejected unconsolidated deltas in the harness.
- Scan of new source and harness: zero `.unwrap()`, `.expect()`, `TODO`, `todo!`, `unimplemented!`, or `unreachable!` occurrences. Existing shared oracle support is reused unchanged.

Every compared row difference and each logged settle count is retained in [initial results](../crates/ivm-duckdb/tests/results/0_first.txt) and [confirmation results](../crates/ivm-duckdb/tests/results/1_confirmation.txt). Repeated identical diagnostic lines from the initial harness output are deduplicated in the text artifact; no differing row is removed.

| Run | Shared cases passed | Shared cases failed | Logged settles | Statements | Source rows in | JSON rows out | Refreshes | Row differences |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Initial | 15 | 19 | 190 | 33,112 | 4,427 | 9,120 | 849 | 12,194 |
| Confirmation | 26 | 8 | 201 | 49,445 | 4,493 | 7,413 | 929 | 10,206 |

Counts aggregate the logged corpus and string frontier events, including expected rejected source changes. The extra post-mark rewind probe is outside these work totals. The four large program fixtures run one empty settle each; these are not registry builds. `7/program.json` and `59/program.json` contain empty captured frontier arrays and are labeled install-only in the results. `16_string.sql` supplies schema without frontier steps and is exercised by the separate lifecycle test. `13_delay` passes by matching DD’s explicit unsupported error.

| Case | Confirmation |
| --- | --- |
| 0_access | FAIL: 42 row/error differences |
| 10_team_sum | PASS |
| 11_recursive_antijoin | PASS |
| 12_window | PASS |
| 13_delay | PASS |
| 14_recursive_antijoin | PASS |
| 15_recursive_mint | PASS |
| 17_nonmonotone_walk | PASS |
| 18_nested_wave_reach | PASS |
| 1_team_cost | PASS |
| 2_weights_access | FAIL: 37 row/error differences |
| 3_weights_team_cost | PASS |
| 4_antijoin | PASS |
| 5_self_join | PASS |
| 6_topk | FAIL: 15 row/error differences |
| 7_reach | PASS |
| 8_timing_and_cells | FAIL: 10029 row/error differences |
| 9_depth_cap | PASS |
| pokemon/0_can_surf | FAIL: 42 row/error differences |
| pokemon/1_party_stats | PASS |
| pokemon/2_party_size | PASS |
| pokemon/3_rematch | PASS |
| pokemon/4_two_roads | PASS |
| pokemon/5_leads | FAIL: 15 row/error differences |
| pokemon/6_walk | PASS |
| pokemon/7_rare_candy | PASS |
| 10_16_intern_row_reuse_case.json | FAIL: 12 row/error differences |
| 59/program.json | PASS |
| 7/program.json | PASS |
| 8_c15_program.json | PASS |
| 9_3_count_case.json | FAIL: 14 row/error differences |
| install/1_macro_library.json | PASS |
| install/2_macrotime.json | PASS |
| install/3_registry_comptime.json | PASS |

First failure: `0_access/0` should emit `(1,100,+1)` and `(3,300,+1)` for relation 3; DuckDB emits neither. The remaining failed cases are `2_weights_access`, `6_topk`, `8_timing_and_cells`, `pokemon/0_can_surf`, `pokemon/5_leads`, and the two captured populated sprefa cases.

No changes were made to ivm-ir or the Engine trait. Normal Cargo dependencies contain no DuckDB crate, driver manager, or DuckDB library.

## Sprefa

Separate branch `feature/ir-duckdb`, commit `1507864c825ccd92f45f56fec5a176707decdd29`: dependency and Backend dispatch wiring are present. Changes are restricted to `Cargo.toml`, `Cargo.lock`, and `src/_6_eval/_9b_ir_eval.rs`. The engine is selected by `DL8_ENGINE=ir:duckdb`. Registry build not run separately.

Both suite executions use the pinned CLI and extension, `CARGO_BUILD_JOBS=4`, and `cargo nextest run -j 4 --offline --locked --no-fail-fast --test it`. SQLite emission tests reuse the existing primary artifact through `SQLITE_IVM_LIB=/Users/chrishafley/projects/sqlite_ivm/target/extension/release/libsqlite_ivm.dylib`; it was not rebuilt. The branch declares the single integration target `it`; its justfile recipes referencing other test targets are stale and were left unchanged.

The initial suite reported 112 passed, 90 failed, and 3 skipped. Its first failure was `_0_read_lower_check::macrotime_cases_share_one_engine`. A separate manual compile probe and DD control after that suite identified an `Unsupported("Mfp Real/Any SQL cell conversion")` install error in the then-current adapter. These probes were extra diagnostic commands, not suite reruns. The engine cleanup removed that blanket conversion guard for pure projection. The wiring received two cleanup reads, with no findings.

Confirmation run `94de8917-a02b-4262-bcda-1691fb84ba43` reported 147 passed, 42 failed, 13 timed out, and 3 skipped. Thus 55 of the 202 executed tests failed, including timeouts. Skipped tests are excluded from passes. The complete output is retained in [sprefa confirmation results](../crates/ivm-duckdb/tests/results/2_sprefa_confirmation.txt).

The first unsuccessful case is `_0_read_lower_check::every_macrotime_case_matches_v7`, which timed out. The first assertion failure is `_1_compile_oracles::every_reify_case_matches_v7`: `case-3_emit_dl7_artifacts_5.json` expects an artifact containing `(1, alpha)` and receives an empty artifact. Host/store cases include compile-duration assertions. The `_9j_no_recursion` static gate also fails on the adapter's recursive `expr` and `recurse` functions. Some tests select DD or SQLite explicitly, so suite passes under the outer DuckDB environment do not establish DuckDB coverage for every test. No additional tests or probes followed confirmation.

## Empty-settle work for saved program fixtures

| Fixture | Statements | Rows in | JSON rows out | Refreshes |
| --- | ---: | ---: | ---: | ---: |
| c15 | 9,180 | 0 | 39 | 0 |
| macro library | 2,528 | 0 | 19 | 0 |
| macrotime | 2,567 | 0 | 19 | 0 |
| registry comptime | 9,410 | 0 | 18 | 0 |

`otool -L` on the final Rust conformance executable lists only `libiconv` and
`libSystem`; no DuckDB dynamic library. Cargo's normal dependency tree also has
no DuckDB implementation crate. The C++ build is owned by the external checkout.
