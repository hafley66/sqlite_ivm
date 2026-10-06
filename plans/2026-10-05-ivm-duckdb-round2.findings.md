# ivm-duckdb: OpenIVM bag lowering, round 2

## Contract and inspected sources

The pinned build is unchanged: DuckDB v1.5.2 (`8a5851971fae891f292c2714d86046ee018e9737`), OpenIVM `cec35ae41e930a607db774673310e6cc263775bd`. Rust still uses the external CLI, SQL stdin and framed JSON stdout. No DuckDB crate or library is linked.

Read in the pinned OpenIVM checkout: `docs/limitations.md`, `docs/internals/parser.md`, `docs/internals/linearity.md`, `docs/internals/metadata-columns.md`, `docs/internals/delta-tables.md`, `docs/operators/*.md`, `docs/refresh/pipelines.md`, and `docs/optimizations/companion-rows.md`. Checked classification against `src/core/incremental_checker.cpp` and `src/core/parser.cpp`; checked output delta retention against `src/upsert/refresh_sql.cpp`.

The authoritative assigned type is `SELECT type FROM openivm_views WHERE view_name = ...`. Codes 0 through 9 map to AGGREGATE_GROUP, SIMPLE_AGGREGATE, SIMPLE_PROJECTION, FULL_REFRESH, AGGREGATE_HAVING, WINDOW_PARTITION, GROUP_RECOMPUTE, TOP_K, DISTINCT_INCREMENTAL, and SEMI_ANTI_RECOMPUTE. Creation profiling exposes `create_compile_classification` details in `openivm_refresh_profile`. The profile detail contains classification and plan counts, not the internal unsupported-reason enum list. Creation warnings are captured verbatim; a FULL_REFRESH assignment without a creation explanation is an install error.

## Lowering and state

Positive nonrecursive nodes use ordinary SQL bags. Mfp uses SELECT/WHERE; Union uses UNION ALL; Join uses CROSS JOIN plus equality predicates; Reduce uses native COUNT/SUM/MIN/MAX; Threshold uses SELECT DISTINCT; Antijoin uses the projection ANTI JOIN shape; TopK uses partitioned ROW_NUMBER with QUALIFY; Window uses native window expressions. OpenIVM owns multiplicity, auxiliary state, and refresh strategy. There are no adapter requests for full refresh. Empty-column bags use a unit column to preserve cardinality.

Let relations are SQL aliases of their node result. They no longer delete and reinsert the entire relation at every settle. Stacked MVs therefore consume OpenIVM's own source and companion deltas.

Each bag output has a projection delta MV and a downstream snapshot MV. The snapshot MV is a real consumer: OpenIVM retains the projection's delta until the adapter reads it, then refreshing the snapshot advances the consumer cursor and permits native delta cleanup. Engine snapshots read that snapshot MV. Signed delta rows are consolidated for the Engine return value; the adapter does not subtract complete bag snapshots. The timestamp filter is a cursor boundary, not a performance measurement.

LetRec bodies retain the explicit simultaneous round loop. Their input node closure uses weighted SQL tables. Persistent negative IR weights cannot be stored as negative row counts in a SQL bag, so Negate and weighted consumers have logged SQL recomputation boundaries. Dictionary scalar ordering and Rust Mint/string evaluation also remain explicit boundaries. Canonical weighted read views convert bag multiplicity to counts only when those boundaries need it. These boundaries are distinct from OpenIVM FULL_REFRESH assignments and are included in the logs.

Real/Any arithmetic, same-rank scalar comparisons, typed join keys, and typed reduce/window inputs retain the explicit unsupported errors from round 1. Delay matches DD's unsupported result. The IR and Engine trait are unchanged.

## Validation

Sequence: implementation, first corpus execution, two cleanup passes, one confirmation execution. Initial result: 30 shared cases passed and 4 failed of 34; the separate string/NUL/rewind lifecycle test passed. Confirmation: 32 passed / 2 failed of 34 shared cases, plus a passing string/NUL/rewind lifecycle test. Two passing cases are install-only because their captured frontier arrays are empty. Delay passes by matching DD's explicit unsupported result. The corpus was run twice, with the two cleanup passes between executions; no implementation changes or further test executions followed confirmation. Status: blocked on the two recorded failures.

Cleanup pass 1: removed the Join-only constructor-read shortcut and implemented constructor feedback until all readers quiesce, following DD's `drain_pending` settle loop. Moved the remaining boundary diagnostic from stdout to stderr. Preserved the full unmatched-settle error payload in the harness instead of reporting only an error count.

Cleanup pass 2: retained classification records even when a missing FULL_REFRESH explanation makes install fail; stopped forcing empty-delta work and restored OpenIVM's default skipping; limited profiling to creation, including after database restoration; preserved errors while reading CLI-exit diagnostics; logged malformed-delta and tick diagnostics and the extra mark/rewind settle. Both passes scanned for unwraps, silent fallbacks, stubs, test-specific branches, and dead branches. No `.unwrap()`, `.expect()`, TODO, `todo!`, `unimplemented!`, or `unreachable!` occurs in the adapter or its conformance harness. Destructor process cleanup is best effort.

Constructor rows minted during evaluation feed every constructor reader before a settle returns. The adapter repeats stratum evaluation when a read constructor grows; output deltas are collected after this quiescence. Dictionary IDs remain stored in SQL tables.

Command for each execution:

```sh
IVM_DUCKDB_CLI=/Users/chrishafley/projects/openivm/build/release/duckdb \
IVM_DUCKDB_EXTENSION=/Users/chrishafley/projects/openivm/build/release/extension/openivm/openivm.duckdb_extension \
CARGO_TERM_PROGRESS_WHEN=never cargo test -p ivm-duckdb -j 4 -- --nocapture --test-threads=1
```

`WORK` records contain statements sent, accepted source rows, JSON rows received, and successful explicit refreshes for each attempted settle. OpenIVM's internal SQL is outside the statement count. Failed installs have classification records for each view created before the error. No registry build or sprefa suite is part of this round.

## Remaining confirmation failures

`9_3_count_case.json/3` fails while refreshing Mfp node 55. OpenIVM assigned SIMPLE_PROJECTION at creation, then `PRAGMA refresh('n55')` returned `Not implemented Error: Plan contains single node, this is not supported`. The exception text originates in the pinned checkout's `src/rules/incremental_rewrite_rule.cpp:124`. The emitted SQL is:

```sql
SELECT s.c0 AS c0, s.c0 AS c1, s.c0 AS c2
FROM n54 s
WHERE ((0)::BIGINT) <> 0
```

The adapter propagates this error and restores the previous database. The resulting eight snapshot differences are retained, including relation 43 row `[1]`, where DD has weight 99 and the restored DuckDB snapshot has weight 43. The assigned type remains SIMPLE_PROJECTION. This case remains failed.

The other failure is `install/3_registry_comptime.json`: the CLI exited during `CREATE MATERIALIZED VIEW n3639`, a UNION ALL with 143 branches. No stderr explanation or exit status was captured. No refresh type was observed for that incomplete creation, and no cause is inferred. Views created earlier in that install are included below. The fixture never reached settle in confirmation.

## Assigned OpenIVM refresh types, confirmation

| IR operator / output stage | Created and observed views | Count per assigned refresh type |
| --- | ---: | --- |
| Get | 1669 | SIMPLE_PROJECTION: 1669 |
| Join | 217 | SIMPLE_PROJECTION: 217 |
| Mfp | 802 | SIMPLE_PROJECTION: 802 |
| Union | 64 | SIMPLE_PROJECTION: 64 |
| Threshold | 60 | AGGREGATE_GROUP: 60 |
| Output | 132 | SIMPLE_PROJECTION: 132 |
| OutputSnapshot | 132 | SIMPLE_PROJECTION: 132 |
| Reduce | 9 | AGGREGATE_GROUP: 6; SIMPLE_AGGREGATE: 3 |
| Window | 7 | WINDOW_PARTITION: 7 |
| Antijoin | 44 | SEMI_ANTI_RECOMPUTE: 44 |
| TopK | 6 | WINDOW_PARTITION: 6 |
| **Total** | **3142** | **FULL_REFRESH: 0** |

No completed, observed view was assigned FULL_REFRESH, so there are no OpenIVM FULL_REFRESH reasons to list. The incomplete registry Union creation has an unknown assignment. Zero FULL_REFRESH assignments does not measure all recomputation: WINDOW_PARTITION uses partition recompute, including whole-input recompute for an empty partition-key list; grouped extrema may recompute affected groups.

The adapter also recorded these SQL/scalar boundaries, excluded from the OpenIVM MV count:

| Boundary reason | Nodes |
| --- | ---: |
| LetRec round body | 3951 |
| weighted input from an explicit IR recompute boundary | 3074 |
| Rust dictionary/string scalar evaluation; bag output tracked by OpenIVM | 91 |
| dictionary scalar lookup; excluded from OpenIVM NUL-sensitive change tracking | 6 |

The LetRec boundary includes each round body's input-node closure. Weighted-input boundaries conservatively propagate beyond that closure; they have not all been converted back to bags after a positive-weight operator. These remain a limitation of this adapter.

## Work and retained evidence

| Execution | Logged settles | Statements sent | Source rows in | JSON rows out | Refresh calls |
| --- | ---: | ---: | ---: | ---: | ---: |
| Initial | 201 | 30,514 | 4,493 | 11,364 | 2,606 |
| Confirmation | 201 | 38,020 | 4,494 | 12,333 | 4,341 |

These totals cover logged attempted settles, including rejected source changes. Confirmation adds the mark/rewind probe and has no registry settle because its install failed. Therefore the two totals have different event membership. Empty saved-fixture settles are not registry builds.

- [Initial run](../crates/ivm-duckdb/tests/results/3_round2_first.txt): every captured row difference, view classification, boundary, and work record. The initial harness counted six unmatched settle errors without printing their payloads; cleanup corrected that diagnostic omission.
- [Confirmation run](../crates/ivm-duckdb/tests/results/4_round2_confirmation.txt): every created/observed view's SQL and classification, both failure diagnostics, all eight row differences, and all work records.
- [Statements and refreshes per settle](../crates/ivm-duckdb/tests/results/5_round2_work.jsonl): machine-readable records, including input/output row counts.

## Shared cases, confirmation

| Case | Result |
| --- | --- |
| 0_access | PASS |
| 10_team_sum | PASS |
| 11_recursive_antijoin | PASS |
| 12_window | PASS |
| 13_delay | PASS |
| 14_recursive_antijoin | PASS |
| 15_recursive_mint | PASS |
| 17_nonmonotone_walk | PASS |
| 18_nested_wave_reach | PASS |
| 1_team_cost | PASS |
| 2_weights_access | PASS |
| 3_weights_team_cost | PASS |
| 4_antijoin | PASS |
| 5_self_join | PASS |
| 6_topk | PASS |
| 7_reach | PASS |
| 8_timing_and_cells | PASS |
| 9_depth_cap | PASS |
| pokemon/0_can_surf | PASS |
| pokemon/1_party_stats | PASS |
| pokemon/2_party_size | PASS |
| pokemon/3_rematch | PASS |
| pokemon/4_two_roads | PASS |
| pokemon/5_leads | PASS |
| pokemon/6_walk | PASS |
| pokemon/7_rare_candy | PASS |
| 10_16_intern_row_reuse_case.json | PASS |
| 59/program.json | PASS |
| 7/program.json | PASS |
| 8_c15_program.json | PASS |
| 9_3_count_case.json | FAIL |
| install/1_macro_library.json | PASS |
| install/2_macrotime.json | PASS |
| install/3_registry_comptime.json | FAIL |
