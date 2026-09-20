---
created: 2026-09-20
updated: 2026-09-20
type: task
status: open
priority: normal
related: ['@json-text-keys-in-indexes', '@lab-wide-table-harness']
labels: [lab]
lane: lab-rig
size: L
collision: [bench/**]
---

# Lab 10: the gang counts the copies

## Description

## Description

sqlite_ivm stores one source row's content 3 + 2·fanout times for a join feeding a
group (`probes/2026-09-20-row-amplification/RESULTS.md`). Each arrangement row holds
the tuple as `c0..cN` and again serialized in `__c` (`src/1a_relational.rs:360`, `:560`).
The group-input arrangement carries every join column because the planner pushes
`Kind::Group` directly on the join node (`src/0b_relational.rs:1339-1350`).

| fanout | copies of `a.id=1` | `chain_op6_0` rows |
|---|---|---|
| 1 | 5 | 2,000 |
| 10 | 23 | 20,000 |
| 40 | 83 | 80,000 |
| 100 | 203 | 200,000 |

This lab measures the same number for differential dataflow, DBSP and pg_ivm on the
shared circuit catalog, through the existing shootout rig.

## What the other engines store

Read from source. dd `~/.cargo/registry/src/*/differential-dataflow-0.25.1`, DBSP
`~/.cargo/registry/src/*/dbsp-0.337.0`, pg_ivm 1.15 `github.com/sraoss/pg_ivm` master
(`pg_ivm.control:3`).

| question | differential dataflow | DBSP | pg_ivm |
|---|---|---|---|
| join output materialized? | no, streamed: `join_traces` returns `Stream` `src/operators/join.rs:63`, emitted `:328` | no, streamed: `dyn_join_generic` returns `Stream` `src/operator/dynamic/join.rs:650-656`, two `JoinTrace` ops `:714`, `:731` summed `:747` | yes: the IMMV is a heap table `createas.c:140`, rows are the query output plus `__ivm_count__` `createas.c:568`; delta applied by UPDATE count / INSERT `matview.c:3760` |
| input side stored whole or referenced? | whole, one arrangement per side: `join_map` arranges both `src/collection.rs:1157-1158`; `arrange_core` inserts sealed batches `src/operators/arrange/arrangement.rs:352`, `:476-478` | whole, one trace per side: `dyn_shard_accumulate_trace` `join.rs:704`, `:708`, defined `src/operator/dynamic/accumulate_trace.rs:67` | referenced: delta joins run against base tables filtered to pre-update state `matview.c:1501`, `:1716`, `WHERE pgivm.ivm_visible_in_prestate(...)` `:1746`; transition tuplestores `:1025`, `:1032` are freed per statement `:1445`, `:1450` |
| per-row identity beside the row? | none: `OrdValStorage { keys, vals, upds }` `src/trace/implementations/ord_neu.rs:266-273`, multiplicity in `upds.diffs` `:112-118` | none: `Layer { keys, offs, vals }` `src/trace/layers/layer.rs:59-77`, leaf `keys`, `diffs` `src/trace/layers/leaf.rs:26-30` | none: `__ivm_count__` per distinct output tuple `createas.c:568` |
| content once or more per arrangement? | once per batch; key once per batch across its values `ord_neu.rs:266-273`; a spine holds several batches `spine_fueled.rs:84-89`; an in-progress merge holds both inputs `spine_fueled.rs:718-728`, visited by `map_batches` `:214` | once per batch; slot holds `merging_batches` and `loose_batches` `src/trace/spine_async.rs:212-217`, merge inputs released `:493` | once per distinct output tuple; input content zero times outside the source table |

Aggregates. dd `reduce_trace` takes an arranged input `src/operators/reduce.rs:93` and
writes an output arrangement `:229`; `count_total` arranges by self `src/operators/count.rs:50`
and streams its output `:97`. DBSP `dyn_aggregate_generic` keeps the input trace
`src/operator/dynamic/aggregate.rs:491` and the output trace through `upsert` `:498`,
`src/operator/dynamic/upsert.rs:92-117`; design note `aggregate.rs:783-793`.

For `aggregate_churn` as spelled at `bench/shared/34_circuit_dd.rs:106-110`, a `b` row's
content lives in one dd arrangement. `explode` projects it away before `count_total`.
The expected dd copy count is 1 steady, 2 during a merge.

## The metric

`probe_copies`: stored cells equal to the marker value across every relation the engine
owns. A cell counts when its integer value equals the marker or its text contains the
marker. Three buckets per engine per state:

| bucket | relations |
|---|---|
| `source` | `a`, `b`, `c`. Always 1 |
| `intermediate` | arrangements, traces, delta relations, dictionaries. The headline |
| `result` | the output relation. Circuit dependent, reported, never the headline |

Secondary, per engine, own units: `bytes` per relation (already in `state_inventory`),
`writes` per state (rows written to owned relations by one state's mutation).

Marker: `999983`. Fixture values are bounded to `|v| <= 1000000`
(`bench/shared/30_circuit_workload.mjs:70`); `a.v` is in -3..3, `b.v` in -1..1, `c.v` in 0..3.
No fixture cell equals the marker.

## Fixture

Add `makeProbeFixture(family, rows, batch, fanout, domain)` to
`bench/shared/30_circuit_workload.mjs` and the semantic twin in `36_semantic_catalog.mjs`.
Two states:

| state | writes | oracle |
|---|---|---|
| `initial` | same as `makeCircuitFixture` | same |
| `probe_row` | two-table shapes: `{table:'b', id:4, row:[4,0,999983]}`. One-table shapes: `{table:'a', id:1, row:[1,0,999983]}` | `circuitOracle` recomputed, sha256 as today |

`b.id=4` has `k=0` and matches every `a` row with `k=0`, which is `fanout` rows
(`30_circuit_workload.mjs:76`). Shapes that join on `b.v` (`chain`, `diamond`) match zero
`c` rows for the probe; the receipt records the observed join partner count from the oracle.

## Cells

Profile `amplification` in `bench/shared/12_crossover_runner.mjs` `buildCases`:

| rows | batch | fanout | expected sqlite-ivm `intermediate` on `aggregate_churn` |
|---|---|---|---|
| 400 | 10 | 10 | 22 |
| 400 | 10 | 200 | 402 |
| 12000 | 10 | 200 | 402 |

Expected is 2 + 2·fanout: two in the `b`-side arrangement, two per join partner in the
group input. The rig reproducing these two numbers is the calibration gate.

## Per-engine work

Every adapter emits `probe_copies` inside the existing `state_inventory` per relation
(`{value, unit:'cells', unavailable_reason}`) and a `probe_writes` metric per mutation.

| arm | adapter | copy count method | writes method |
|---|---|---|---|
| `sqlite-plugin-delta` | `31b_circuit_sqlite.py`, `30a_state_inventory.py` | per owned table, per column: `sum(CASE typeof(col) WHEN 'integer' THEN col=999983 WHEN 'text' THEN instr(col,'999983')>0 ELSE 0 END)`. Reference: `probes/2026-09-20-row-amplification/measure.py` | `connection.total_changes` delta minus `affected_rows` |
| `sqlite-query` | `31b_circuit_sqlite.py` | same scan; owned tables are none, so `intermediate` is 0. Floor | 0 |
| `dd` | `34_circuit_dd.rs`, `38a_semantic_dd.rs` | spell every circuit with explicit `arrange_by_key()` and keep each `Arranged.trace` handle; joins via `join_core`; aggregates via `reduce_abelian` or `count_total_core` on the arranged input so the output trace handle is kept too. After each `step_while`, `trace.map_batches` and cursor-walk keys and vals counting fields equal to the marker. Report `batches` per trace beside `probe_copies`; `map_batches` visits both inputs of an in-progress merge (`spine_fueled.rs:214`), so a count above steady state with `batches > 1` is a merge in flight. Advance `set_logical_compaction` and `set_physical_compaction` on kept handles after each state so merges proceed | records in batches sealed during the state, from the same walk |
| `pg_ivm`, `pglite-ivm` | `31a_circuit_postgres.mjs`, `30b_postgres_inventory.mjs` | relation set: `circuit_view`, every relation in `pgivm` and `pg_my_temp_schema()` after the statement. Per column `count(*) FILTER (WHERE col::text = '999983' OR col::text LIKE '%999983%')`. Expected `intermediate` 0 for all shapes; the catalog scan proves it | `pg_stat_user_tables` `n_tup_ins + n_tup_upd + n_tup_del` on `circuit_view`, after `pg_stat_force_next_flush()` as `30b_postgres_inventory.mjs:7` does |
| `query`, `pglite-query` | `31a_circuit_postgres.mjs` | same scan; `intermediate` 0. Floor | 0 |
| `swi-circuit` | excluded from this profile | no state inventory | |
| `dbsp` | new, see below | | |

## DBSP

In scope. Ordered last. Cost:

| item | file |
|---|---|
| dependency `dbsp = "=0.337.0"` | `bench/Cargo.toml:8` neighborhood |
| new bin `circuit_dbsp` | `bench/shared/34b_circuit_dbsp.rs`, mirror of `34_circuit_dd.rs` receipt for receipt |
| new bin `semantic_dbsp` | `bench/shared/38b_semantic_dbsp.rs` |
| arm registration | `12_crossover_runner.mjs` arms and `--circuit-dbsp-bin`; `51_shootout_report.mjs:5-6` `engines`, `arms`; `53_shootout.mjs` build and `circuitArgs` |
| compile time of the dbsp crate | record wall seconds of the first `cargo build --release --manifest-path bench/Cargo.toml` in the receipt |

Spelling: typed API. `join_index` `src/operator/join.rs:185`, `aggregate_linear`
`src/operator/aggregate.rs:209`, `distinct` `src/operator/distinct.rs:38`, `recursive`
`src/operator/recursive.rs:262`, `topk_asc` `src/operator/group/topk.rs:20`. Window
shapes with no operator are reported unsupported.

Copy count: the join's internal traces are private. Spell each join input and each
aggregate input with an explicit `integrate_trace()` (`src/operator/trace.rs:290`) on the
same stream, count in that mirror by cursor walk inside `inspect`, and report the mirror
as the trace's content. Cross-check record totals against `retrieve_profile()`
(`src/circuit/dbsp_handle.rs:2326`) per operator; a disagreement is reported, never
reconciled by hand.

## Unsupported shapes

An arm that cannot spell a shape emits the existing `capability` event with
`status:'unsupported'` and a `reason` naming the missing operator. The report row shows
`unsupported` and the reason. No arm is handed another arm's dialect: pg_ivm gets the
catalog SQL through `pgivm.create_immv`, dd and DBSP get Rust, sqlite_ivm gets the
catalog SQL through `sqlite_ivm_create`.

## Report

`55_shootout_plot.mjs` `inventory.tsv` gains columns `probe_copies_source`,
`probe_copies_intermediate`, `probe_copies_result`, `probe_writes`. The lane writes
`bench/56_amplification.md`: one table, rows engine × shape × cell, columns the four
above plus `bytes_intermediate`, plus the dd `batches` column. Unsupported rows carry
the reason in place of numbers.

## Validation command

```bash
node bench/53_shootout.mjs amplification --engines sqlite-ivm,sqlite-query,dd,pg-ivm,pg-query,dbsp --out bench/results/amplification
node --test bench/52_shootout.test.mjs bench/55_shootout_plot.test.mjs bench/shared/33_circuit.test.mjs
python3 bench/shared/27_sqlite_ivm.test.py -v
```

Exit 0 and `bench/56_amplification.md` written.

## Acceptance Criteria

- [ ] `makeProbeFixture` exists for both catalogs; `probe_row` state has a sha256 from the JS oracle; no fixture cell equals `999983` before `probe_row`
- [ ] sqlite-ivm `aggregate_churn` reports `probe_copies_intermediate` 22 at `400:10:10` and 402 at `400:10:200`
- [ ] sqlite-ivm reports `probe_copies_source` 1 in every cell
- [ ] dd reports `probe_copies_intermediate` per arrangement with a `batches` count beside it, for all 20 shapes or an `unsupported` reason
- [ ] pg-ivm reports `probe_copies_intermediate` 0 with the relation list that was scanned, for all shapes it accepts; rejected shapes carry pg_ivm's error text as the reason
- [ ] dbsp arm builds, runs the profile, and reports mirror counts plus `retrieve_profile` record totals side by side
- [ ] `sqlite-query` and `pg-query` report `probe_copies_intermediate` 0 as the floor
- [ ] `inventory.tsv` has the four new columns; `bench/56_amplification.md` written by the validation command
- [ ] existing `smoke` profile output unchanged: `node bench/53_shootout.mjs smoke` exit 0

## Test Plan

**What breaks if wrong:** a scan that misses a relation reports a low copy count and the
comparison flatters one engine. A marker that collides with fixture data inflates it.

**Units under test:** the per-engine scan functions and the fixture builder. Pure
computation, unit tests allowed for those. Everything else runs through the rig binaries.

| case | input | expected | why it exists |
|---|---|---|---|
| calibration | sqlite-ivm `aggregate_churn` `400:10:10` | intermediate 22 | ties the rig to `probes/2026-09-20-row-amplification/RESULTS.md` |
| fanout tracks | same at `400:10:200` | 402 | copies must scale 2·fanout or the scan misses the group input |
| marker is unique | every state's `inputs` before `probe_row` | zero cells equal to `999983` | a collision makes every count wrong |
| every table scanned | sqlite-ivm relation list in the receipt | equals `__ivm_objects` tables for the view plus `a`, `b`, `c` | a skipped `_keys` or `_state` table hides copies |
| pg_ivm keeps nothing | pg-ivm relation list after `probe_row` | only `circuit_view` plus the sources; `pgivm` and temp schema empty | the claim at `matview.c:1445`, `:1450` becomes a receipt |
| dd merge visible | dd trace with `batches > 1` | `probe_copies` at most 2 × steady | `spine_fueled.rs:214` double-visit is reported, never averaged away |
| result bucket separate | `topk` with probe in `a` | `result` 1, `intermediate` per engine | the marker reaching the output must never inflate the headline |
| unsupported path | pg-ivm on `window_rank` | `unsupported` with pg_ivm's error text | a shape an engine rejects is a row, never a crash |

**Untested and why:** wall time. The lab is about counts and bytes. Concurrency: SQLite
is single-writer; the rig runs one adapter at a time.

## Receipts

- `bench/results/amplification-*/run.json`, `circuits.jsonl`, `charts/inventory.tsv`
- `bench/56_amplification.md`
- one `boop beep` line per engine landed, with its `probe_copies_intermediate` for `aggregate_churn 400:10:10`
