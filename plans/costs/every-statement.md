# the statement counts, every statement this engine runs

One table per scenario: phase, verb, site, object, calls, milliseconds,
microseconds per call, p99, rows touched, share prepared from the statement
cache, and calls per input row. Sorted by the last column. The tsv beside this
page is `plans/costs/every-statement.tsv`.

Command, three runs of the counts plus the wall pass in two builds:

```
bash scripts/every-statement.sh
```

Which build each column comes from:

- Calls, rows, and microseconds come from hafley-observe's SQLite trace, read
  under the default feature set. The microseconds are SQLite's own profile
  clock, not the statement spans: the spans name site, phase and object, they
  do not wrap the clock. The `--no-default-features --features bundled` build
  reports the same microseconds.
- Wall milliseconds are logging-off runs, printed for both the statements build
  and the spans-compiled-out build. Three runs, median with the min and max
  beside it. The denominator is the median over three runs.
- A cell whose median total falls below 1000 us reads "no effect at this
  scale": that is the granularity of this SQLite build's profile clock
  (`docs/failure-modes.md`, "statement nanos read as 0 or 1000000").
- `rows_touched` is `changes()` for a DML statement and the returned row count
  for a query. It is blank when the site does not know the count: a multi
  statement `execute_batch`, a collector call, and a pragma each leave it
  unrecorded rather than reporting a false zero.

## what this engine wastes

**A statement per transaction where one per batch would do.** Real. One hundred
inserts in one transaction run 225 statements. The same one hundred inserts in
one hundred transactions run 3492, 15.5x. Per input row that is 2.25 against
34.9. The drain is set-at-a-time inside a batch (`tests/13_statements_per_drain.rs`
holds those counts constant across batches); the waste is that the engine
drains once per transaction. Drain-phase statements go 107 to 800, maintain 21
to 2100, materialize 17 to 413.

**The statements that repeat per transaction.** Real, and named:

| site | one transaction | one hundred transactions | per input row |
|---|---:|---:|---:|
| `src/1d_drain.rs` sweep DELETE | 4 | 400 | 4.00 |
| `src/1d_drain.rs` touched SELECT | 3 | 300 | 3.00 |
| `src/1d_drain.rs` arrangement upsert INSERT | 3 | 300 | 3.00 |
| `src/1d_drain.rs` arrangement upsert DELETE | 3 | 300 | 3.00 |
| `src/1c_materialize.rs` node materialize INSERT | 5 | 203 | 2.03 |
| `src/1c_materialize.rs` fixpoint copy INSERT | 3 | 201 | 2.01 |

Each runs once per node per transaction. Nothing in the statement depends on
which row changed. Twenty statements per batch would carry the same rows.

**Statement prepared fresh that the cache could serve.** Real, one-shot per
view. Declaring the group circuit prepares 88 statements fresh (`prepared_pct`
0.0): the catalog INSERT and SELECT in `src/0a_catalog.rs`, the source probes in
`src/0c_compile_from.rs`, the state DDL and the populate INSERT/SELECT in
`src/1b_state.rs`, and the four out-table DELETEs at `src/1b_state.rs`. The
reach circuit prepares 170. `src/0a_catalog.rs` alone prepares the same SELECT
ten times. The drain and maintain path is 100% cached. Median total is under
1000 us per cell, so no measured effect at this scale.

**DDL issued where nothing changed.** Real. `CREATE VIRTUAL TABLE` on an empty
group circuit issues 92 statements, 32 of them `CREATE`, before any row exists:
one state table and dictionary, one arrangement table and two indexes, two
indexes per node, one shadow, three source triggers per source, and the catalog
tables. Nothing changed; the DDL is the shape of the plan.

**A table declared that no query reads.** Real. The circuit has four nodes, so
the out-table CREATE at `src/1b_state.rs` runs four times and
`src/1e_program.rs` declares four `temp.__ivm_before_*` tables. One node is the
arrangement; the other three `before` tables are written by nothing and read by
nothing.

**A statement in a loop whose body does not depend on the loop.** Real, same
evidence as the first row: the per-transaction drain repeats a fixed statement
set. Inside one drain the touched probe runs once per input side and the sweep
once per node, independent of the changed row.

**Not wasteful, for the record.** Inside one batch the fixpoint and the
arrangement run a constant number of statements per node. `retract_recursive`
runs 72 fixpoint statements to retract a closure over 100 edges; `reach_small`
runs 36 over eight. Both stay flat as the batch grows.


## create_group

| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| declare | CREATE | src/1d_drain.rs:23 | g | 11 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 11.00 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:79 | g | 10 | no effect at this scale | 0.0 | 0.0 | 10 | 0.0 | 10.00 | 0.0 |
| declare | SELECT | src/0a_catalog.rs:71 | g | 10 | no effect at this scale | 0.0 | 0.0 | 10 | 0.0 | 10.00 | 0.0 |
| declare | CREATE | src/0a_catalog.rs:26 | g | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 6.00 | 1000.0 |
| declare | CREATE | src/1b_state.rs:195 | g | 4 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 4.00 | 0.0 |
| declare | DELETE | src/1b_state.rs:205 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 4.00 | 0.0 |
| materialize | DELETE | src/1b_state.rs:249 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 4.00 | 0.0 |
| declare | CREATE | src/1a_relational.rs:270 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 3.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:82 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 3.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:62 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 3.00 | 0.0 |
| declare | SELECT | src/1a_relational.rs:200 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 3.00 | 0.0 |
| declare | SELECT | src/1b_state.rs:17 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 3.00 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:377 | g | 3 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 3.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:56 | g | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | a | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | CREATE | src/1a_relational.rs:208 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:44 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 0.0 |
| declare | CREATE | src/2_vtab.rs:351 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 0.0 |
| declare | EXPLAIN | src/0b_relational.rs:626 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 48 | 0.0 | 1.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:45 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:53 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | INSERT | src/2_vtab.rs:354 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | a | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/1b_state.rs:177 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/2_vtab.rs:237 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/2_vtab.rs:341 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| materialize | INSERT | src/1b_state.rs:309 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.00 | 0.0 |
| materialize | INSERT | src/1b_state.rs:319 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.00 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:437 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:329 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:360 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:372 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.00 | 0.0 |

## inserts_many_txn

| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| drain | DELETE | src/1d_drain.rs:112 | g | 400 | no effect at this scale | 0.0 | 0.0 | 400 | 100.0 | 4.00 | 1000.0 |
| drain | SELECT | src/1d_drain.rs:71 | g | 300 | no effect at this scale | 0.0 | 0.0 | 300 | 100.0 | 3.00 | 1000.0 |
| maintain | DELETE | src/1d_drain.rs:147 | g | 300 | 1.000 | 3.3 | 0.0 | 200 | 100.0 | 3.00 | 2000.0 |
| maintain | INSERT | src/1d_drain.rs:147 | g | 300 | 1.000 | 3.3 | 0.0 | 200 | 100.0 | 3.00 | 2000.0 |
| materialize | INSERT | src/1c_materialize.rs:377 | g | 203 | 1.000 | 4.9 | 0.0 | 200 | 100.0 | 2.03 | 3000.0 |
| materialize | INSERT | src/1c_materialize.rs:437 | g | 201 | 1.000 | 5.0 | 0.0 | 100 | 100.0 | 2.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:215 | catalog | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| drain | INSERT | src/1d_drain.rs:53 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| maintain | DELETE | src/1d_drain.rs:124 | g | 100 | no effect at this scale | 0.0 | 0.0 | 99 | 100.0 | 1.00 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:135 | g | 100 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:181 | g | 100 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 1000.0 |
| maintain | DELETE | src/1d_drain.rs:324 | g | 100 | no effect at this scale | 0.0 | 0.0 | 99 | 100.0 | 1.00 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:332 | g | 100 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:129 | g | 100 | 1.000 | 10.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| maintain | INSERT | src/1d_drain.rs:130 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| maintain | INSERT | src/1d_drain.rs:134 | g | 100 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 1000.0 |
| maintain | INSERT | src/1d_drain.rs:325 | g | 100 | 2.000 | 20.0 | 1000.0 | 100 | 100.0 | 1.00 | 3000.0 |
| maintain | INSERT | src/1d_drain.rs:327 | g | 100 | 1.000 | 10.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| maintain | SELECT | src/1d_drain.rs:178 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 2000.0 |
| maintain | SELECT | src/1d_drain.rs:187 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 1000.0 |
| maintain | SELECT | src/1d_drain.rs:193 | g | 100 | 1.000 | 10.0 | 0.0 | 100 | 100.0 | 1.00 | 2000.0 |
| maintain | SELECT | src/1d_drain.rs:328 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 0.0 |
| maintain | UPDATE | src/1d_drain.rs:326 | g | 100 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| declare | CREATE | src/1d_drain.rs:23 | g | 11 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.11 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:79 | g | 10 | no effect at this scale | 0.0 | 0.0 | 10 | 0.0 | 0.10 | 0.0 |
| declare | SELECT | src/0a_catalog.rs:71 | g | 10 | no effect at this scale | 0.0 | 0.0 | 10 | 0.0 | 0.10 | 0.0 |
| declare | CREATE | src/0a_catalog.rs:26 | g | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.06 | 0.0 |
| declare | CREATE | src/1b_state.rs:195 | g | 4 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.04 | 0.0 |
| declare | DELETE | src/1b_state.rs:205 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.04 | 0.0 |
| materialize | DELETE | src/1b_state.rs:249 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.04 | 0.0 |
| declare | CREATE | src/1a_relational.rs:270 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.03 | 0.0 |
| declare | CREATE | src/1b_state.rs:82 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.03 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:62 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| declare | SELECT | src/1a_relational.rs:200 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| declare | SELECT | src/1b_state.rs:17 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| declare | CREATE | src/1b_state.rs:56 | g | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | a | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.02 | 0.0 |
| declare | CREATE | src/1a_relational.rs:208 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | CREATE | src/1b_state.rs:44 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | CREATE | src/2_vtab.rs:351 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | EXPLAIN | src/0b_relational.rs:626 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 48 | 0.0 | 0.01 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:45 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:53 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | INSERT | src/2_vtab.rs:354 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | a | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/1b_state.rs:177 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:237 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:341 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| materialize | INSERT | src/1b_state.rs:309 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |
| materialize | INSERT | src/1b_state.rs:319 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:329 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:360 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:372 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |

## inserts_one_txn

| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| drain | INSERT | src/1d_drain.rs:53 | g | 100 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 0.0 |
| declare | CREATE | src/1d_drain.rs:23 | g | 11 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.11 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:79 | g | 10 | 1.000 | 100.0 | 1000.0 | 10 | 0.0 | 0.10 | 1000.0 |
| declare | SELECT | src/0a_catalog.rs:71 | g | 10 | no effect at this scale | 0.0 | 0.0 | 10 | 0.0 | 0.10 | 0.0 |
| declare | CREATE | src/0a_catalog.rs:26 | g | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.06 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:377 | g | 5 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 0.05 | 0.0 |
| declare | CREATE | src/1b_state.rs:195 | g | 4 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.04 | 0.0 |
| declare | DELETE | src/1b_state.rs:205 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.04 | 0.0 |
| drain | DELETE | src/1d_drain.rs:112 | g | 4 | no effect at this scale | 0.0 | 0.0 | 400 | 100.0 | 0.04 | 0.0 |
| materialize | DELETE | src/1b_state.rs:249 | g | 4 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.04 | 0.0 |
| declare | CREATE | src/1a_relational.rs:270 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.03 | 0.0 |
| declare | CREATE | src/1b_state.rs:82 | g | 3 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.03 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:62 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| declare | SELECT | src/1a_relational.rs:200 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| declare | SELECT | src/1b_state.rs:17 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.03 | 0.0 |
| drain | SELECT | src/1d_drain.rs:71 | g | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 100.0 | 0.03 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:147 | g | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 0.03 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:147 | g | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 0.03 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:437 | g | 3 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.03 | 0.0 |
| declare | CREATE | src/1b_state.rs:56 | g | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | a | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.02 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.02 | 0.0 |
| declare | CREATE | src/1a_relational.rs:208 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | CREATE | src/1b_state.rs:44 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | CREATE | src/2_vtab.rs:351 | g | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.01 | 0.0 |
| declare | EXPLAIN | src/0b_relational.rs:626 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 48 | 0.0 | 0.01 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:45 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:53 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | INSERT | src/2_vtab.rs:354 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | a | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/1b_state.rs:177 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:215 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:237 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| declare | SELECT | src/2_vtab.rs:341 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:124 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:135 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:181 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:324 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:332 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:129 | g | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.01 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:130 | g | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.01 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:134 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:325 | g | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.01 | 0.0 |
| maintain | INSERT | src/1d_drain.rs:327 | g | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.01 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:178 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.01 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:187 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.01 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:193 | g | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 0.01 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:328 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.01 | 0.0 |
| maintain | UPDATE | src/1d_drain.rs:326 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.01 | 0.0 |
| materialize | INSERT | src/1b_state.rs:309 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |
| materialize | INSERT | src/1b_state.rs:319 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:329 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:360 | g | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.01 | 0.0 |
| materialize | SELECT | src/1b_state.rs:372 | g | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.01 | 0.0 |

## reach_small

| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| declare | CREATE | src/1d_drain.rs:23 | r | 28 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 3.50 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:79 | r | 20 | no effect at this scale | 0.0 | 0.0 | 20 | 0.0 | 2.50 | 0.0 |
| declare | SELECT | src/0a_catalog.rs:71 | r | 20 | no effect at this scale | 0.0 | 0.0 | 20 | 0.0 | 2.50 | 0.0 |
| drain | INSERT | src/1d_drain.rs:53 | r | 16 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 2.00 | 1000.0 |
| materialize | INSERT | src/1c_materialize.rs:377 | r | 14 | 1.000 | 71.4 | 1000.0 | 48 | 100.0 | 1.75 | 1000.0 |
| declare | CREATE | src/1b_state.rs:195 | r | 9 | 1.000 | 111.1 | 1000.0 | 1 | 0.0 | 1.12 | 1000.0 |
| declare | DELETE | src/1b_state.rs:205 | r | 9 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.12 | 1000.0 |
| drain | DELETE | src/1d_drain.rs:112 | r | 9 | no effect at this scale | 0.0 | 0.0 | 72 | 100.0 | 1.12 | 0.0 |
| materialize | DELETE | src/1b_state.rs:249 | r | 9 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.12 | 0.0 |
| drain | SELECT | src/1d_drain.rs:71 | r | 8 | no effect at this scale | 0.0 | 0.0 | 8 | 100.0 | 1.00 | 0.0 |
| declare | SELECT | src/1b_state.rs:17 | r | 7 | no effect at this scale | 0.0 | 0.0 | 7 | 0.0 | 0.88 | 0.0 |
| declare | CREATE | src/0a_catalog.rs:26 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.75 | 1000.0 |
| declare | CREATE | src/1a_relational.rs:270 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.75 | 0.0 |
| declare | CREATE | src/1b_state.rs:82 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.75 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:62 | r | 6 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.75 | 0.0 |
| fixpoint | SELECT | src/1d_drain.rs:223 | r | 5 | no effect at this scale | 0.0 | 0.0 | 5 | 100.0 | 0.62 | 0.0 |
| declare | SELECT | src/1a_relational.rs:200 | r | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 0.38 | 2000.0 |
| declare | CREATE | src/1b_state.rs:119 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.25 | 0.0 |
| declare | CREATE | src/1b_state.rs:141 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.25 | 0.0 |
| declare | CREATE | src/1b_state.rs:56 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.25 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:53 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | a | 2 | 1.000 | 500.0 | 1000.0 | 2 | 0.0 | 0.25 | 1000.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | b | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | b | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | b | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 0.25 | 0.0 |
| declare | SELECT | src/1b_state.rs:177 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.25 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:324 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:332 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:339 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:340 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:164 | r | 2 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:165 | r | 2 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:301 | r | 2 | no effect at this scale | 0.0 | 0.0 | 8 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:314 | r | 2 | no effect at this scale | 0.0 | 0.0 | 8 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:325 | r | 2 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:327 | r | 2 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:341 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:342 | r | 2 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 0.25 | 0.0 |
| fixpoint | SELECT | src/1d_drain.rs:255 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 100.0 | 0.25 | 0.0 |
| fixpoint | SELECT | src/1d_drain.rs:328 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 100.0 | 0.25 | 0.0 |
| fixpoint | UPDATE | src/1d_drain.rs:326 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| materialize | INSERT | src/1b_state.rs:309 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.25 | 0.0 |
| materialize | INSERT | src/1b_state.rs:319 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.25 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:448 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.25 | 0.0 |
| materialize | SELECT | src/1b_state.rs:329 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 0.25 | 0.0 |
| declare | CREATE | src/1a_relational.rs:208 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.12 | 0.0 |
| declare | CREATE | src/1b_state.rs:44 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.12 | 0.0 |
| declare | CREATE | src/2_vtab.rs:351 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 0.12 | 0.0 |
| declare | EXPLAIN | src/0b_relational.rs:626 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 56 | 0.0 | 0.12 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:45 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| declare | INSERT | src/2_vtab.rs:354 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | a | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | b | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| declare | SELECT | src/2_vtab.rs:215 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.12 | 0.0 |
| declare | SELECT | src/2_vtab.rs:237 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| declare | SELECT | src/2_vtab.rs:341 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:239 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.12 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:181 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 0.12 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:178 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.12 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:187 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.12 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:193 | r | 1 | no effect at this scale | 0.0 | 0.0 | 8 | 100.0 | 0.12 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:477 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.12 | 0.0 |
| materialize | SELECT | src/1b_state.rs:360 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 0.12 | 0.0 |
| materialize | SELECT | src/1b_state.rs:372 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 0.12 | 0.0 |
| materialize | SELECT | src/1c_materialize.rs:444 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.12 | 0.0 |
| materialize | SELECT | src/1c_materialize.rs:456 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 0.12 | 0.0 |

## retract_recursive

| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| drain | INSERT | src/1d_drain.rs:53 | r | 201 | no effect at this scale | 0.0 | 0.0 | 201 | 100.0 | 201.00 | 0.0 |
| declare | CREATE | src/1d_drain.rs:23 | r | 28 | 1.000 | 35.7 | 1000.0 |  | 0.0 | 28.00 | 1000.0 |
| declare | INSERT | src/0a_catalog.rs:79 | r | 20 | no effect at this scale | 0.0 | 0.0 | 20 | 0.0 | 20.00 | 0.0 |
| declare | SELECT | src/0a_catalog.rs:71 | r | 20 | no effect at this scale | 0.0 | 0.0 | 20 | 0.0 | 20.00 | 0.0 |
| drain | DELETE | src/1d_drain.rs:112 | r | 18 | no effect at this scale | 0.0 | 0.0 | 902 | 100.0 | 18.00 | 0.0 |
| drain | SELECT | src/1d_drain.rs:71 | r | 16 | no effect at this scale | 0.0 | 0.0 | 16 | 100.0 | 16.00 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:377 | r | 15 | 1.000 | 66.7 | 1000.0 | 601 | 100.0 | 15.00 | 2000.0 |
| fixpoint | SELECT | src/1d_drain.rs:223 | r | 13 | no effect at this scale | 0.0 | 0.0 | 13 | 100.0 | 13.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:195 | r | 9 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 9.00 | 0.0 |
| declare | DELETE | src/1b_state.rs:205 | r | 9 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 9.00 | 0.0 |
| materialize | DELETE | src/1b_state.rs:249 | r | 9 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 9.00 | 0.0 |
| declare | SELECT | src/1b_state.rs:17 | r | 7 | no effect at this scale | 0.0 | 0.0 | 7 | 0.0 | 7.00 | 0.0 |
| declare | CREATE | src/0a_catalog.rs:26 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 6.00 | 0.0 |
| declare | CREATE | src/1a_relational.rs:270 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 6.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:82 | r | 6 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 6.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:62 | r | 6 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 6.00 | 1000.0 |
| declare | SELECT | src/1a_relational.rs:200 | r | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 0.0 | 3.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:324 | r | 3 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 3.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:332 | r | 3 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 3.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:339 | r | 3 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 3.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:340 | r | 3 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:164 | r | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:165 | r | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:239 | r | 3 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:301 | r | 3 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:325 | r | 3 | no effect at this scale | 0.0 | 0.0 | 201 | 100.0 | 3.00 | 1000.0 |
| fixpoint | INSERT | src/1d_drain.rs:327 | r | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:341 | r | 3 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 3.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:342 | r | 3 | no effect at this scale | 0.0 | 0.0 | 200 | 100.0 | 3.00 | 0.0 |
| fixpoint | SELECT | src/1d_drain.rs:255 | r | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 100.0 | 3.00 | 0.0 |
| fixpoint | SELECT | src/1d_drain.rs:328 | r | 3 | no effect at this scale | 0.0 | 0.0 | 3 | 100.0 | 3.00 | 0.0 |
| fixpoint | UPDATE | src/1d_drain.rs:326 | r | 3 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 3.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:119 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 2.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:141 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 2.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:56 | r | 2 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 2.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:53 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | a | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:37 | b | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:47 | b | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | a | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:48 | b | 2 | no effect at this scale | 0.0 | 0.0 | 6 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/1b_state.rs:177 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | SELECT | src/2_vtab.rs:215 | catalog | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 100.0 | 2.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:314 | r | 2 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 2.00 | 0.0 |
| materialize | INSERT | src/1b_state.rs:309 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 2.00 | 0.0 |
| materialize | INSERT | src/1b_state.rs:319 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 2.00 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:448 | r | 2 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 2.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:329 | r | 2 | no effect at this scale | 0.0 | 0.0 | 2 | 0.0 | 2.00 | 0.0 |
| declare | CREATE | src/1a_relational.rs:208 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 0.0 |
| declare | CREATE | src/1b_state.rs:44 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 1000.0 |
| declare | CREATE | src/2_vtab.rs:351 | r | 1 | no effect at this scale | 0.0 | 0.0 |  | 0.0 | 1.00 | 0.0 |
| declare | EXPLAIN | src/0b_relational.rs:626 | catalog | 1 | no effect at this scale | 0.0 | 0.0 | 56 | 0.0 | 1.00 | 0.0 |
| declare | INSERT | src/0a_catalog.rs:45 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 1000.0 |
| declare | INSERT | src/2_vtab.rs:354 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | a | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/0c_compile_from.rs:41 | b | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/2_vtab.rs:237 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| declare | SELECT | src/2_vtab.rs:341 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:259 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:260 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:279 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:296 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | DELETE | src/1d_drain.rs:312 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:263 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:278 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:282 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:294 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:309 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:310 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| fixpoint | INSERT | src/1d_drain.rs:311 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| maintain | DELETE | src/1d_drain.rs:181 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 100.0 | 1.00 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:178 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:187 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| maintain | SELECT | src/1d_drain.rs:193 | r | 1 | no effect at this scale | 0.0 | 0.0 | 100 | 100.0 | 1.00 | 0.0 |
| materialize | INSERT | src/1c_materialize.rs:477 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:360 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 0.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1b_state.rs:372 | r | 1 | no effect at this scale | 0.0 | 0.0 | 0 | 0.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1c_materialize.rs:444 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |
| materialize | SELECT | src/1c_materialize.rs:456 | r | 1 | no effect at this scale | 0.0 | 0.0 | 1 | 100.0 | 1.00 | 0.0 |

## wall, logging off, median of three (min..max)

| scenario | statements build ms | spans compiled out ms |
|---|---:|---:|
| create_group | 0.921 (0.911..1.359) | 0.989 (0.945..2.453) |
| inserts_one_txn | 2.047 (2.044..2.127) | 2.281 (2.211..2.298) |
| inserts_many_txn | 8.574 (8.517..8.652) | 9.490 (9.418..9.509) |
| retract_recursive | 4.863 (4.639..5.701) | 5.703 (5.544..6.013) |
| reach_small | 3.368 (3.147..3.473) | 3.178 (3.143..3.417) |

wrote /Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/feature/every-statement/plans/costs/every-statement.tsv

## every statement site

`src/**` holds 165 statement call sites. Each names a phase and an object; the
verb, kind and `file:line` come from the SQL and the caller.

| file | sites | note |
|---|---:|---|
| `src/0a_catalog.rs` | 13 | catalog DDL and rows |
| `src/0b_relational.rs` | 3 | EXPLAIN and the function probe |
| `src/0c_compile_from.rs` | 5 | pragma_table_list, pragma_table_xinfo, sqlite_schema |
| `src/0d_compile_select.rs` | 0 | no SQL: lowers the AST |
| `src/0e_compile_recursive.rs` | 0 | no SQL: lowers the AST |
| `src/0f_columns.rs` | 0 | no SQL: rewrites column references |
| `src/1a_relational.rs` | 6 | install, hooks, intern, resolve |
| `src/1b_state.rs` | 18 | create_state, populate, fill, write_state |
| `src/1c_materialize.rs` | 14 | per-node materialize, group and fixpoint |
| `src/1d_drain.rs` | 45 | seed, touched, sweep, arrangement, fixpoint, apply_state |
| `src/2_vtab.rs` | 36 | attach, migrate, convert, rename, drain, cursor read |
| `src/2a_source_ddl.rs` | 21 | savepoint, rename, drop_source |
| `src/3_extension.rs` | 4 | create and drop the managed table |

Two sites are not instrumented, with reasons:

- `src/0b_relational.rs:617` `db.prepare(sql)` reads column names and the
  parameter count. It steps nothing, so it issues no statement to SQLite and
  carries no span.
- `src/2_vtab.rs` savepoint, release and rollback forward to the collector.
  The collector issues no SQL on those calls; its shadow DDL, drain read and
  stage write each sit under a `statements::guard` span at the call.

Two calls delegate into instrumented code rather than opening their own span:
`src/1c_materialize.rs` and the three `materialize.execute(db, name)` calls in
`src/1d_drain.rs` enter `MaterializeStatements::execute`, where every statement
is instrumented.

## the SQLite APIs this reads

hafley-observe's `sqlite::instrument` is the source; the spans only add what
SQLite cannot know (phase, site, object). The engine does not re-report sql,
nanos or the counters.

| API | used | source of |
|---|---|---|
| `sqlite3_trace_v2` STMT | yes | the expanded SQL of every statement, via `emit` in `hafley-observe/src/5_sqlite.rs` |
| `sqlite3_trace_v2` PROFILE | yes | nanoseconds per statement, and the counters below |
| `sqlite3_stmt_status` VM_STEP, FULLSCAN_STEP, SORT, AUTOINDEX, REPREPARE, RUN, MEM_USED | yes | `StatementCounters::read`; `tests/18_statement_counts.rs` pins the `vm_step` and `fullscan_step` sums per phase |
| `sqlite3_trace_v2` ROW | no | per-row callbacks cost more than the table needs; `changes()` and the query row count already give the count |
| `sqlite3_stmt_scanstatus_v2` | no | rusqlite exposes no binding, and reaching it means raw FFI in the engine, against the no-bespoke law; `fullscan_step` and `vm_step` flag the same scans at statement granularity |
| `sqlite3_db_status` | no | the counters are per connection; every column here is per statement. `prepared_pct` comes from the call path, not from the cache counters |

`query_map` prepares fresh at every site: it is the mapped catalog read in
`src/0a_catalog.rs`, `src/2_vtab.rs`, `src/2a_source_ddl.rs` and
`src/0c_compile_from.rs`, and each is a one-shot. No cached mapped query
exists, so `prepared_pct` reports them correctly; the day a hot mapped query
appears, `query_map_cached` is the missing helper.

The `statements` feature is in the default set, so the rlib and every test
target carry the 165 sites and `cargo test` exercises them.
`--no-default-features` is the extension build alone, and there the sites
compile out.

## receipts

- R1 site ledger above, plus the API table: trace_v2 STMT and PROFILE and
  stmt_status are used; ROW, scanstatus_v2 and db_status are not, each with a
  reason.
- R2 `bash scripts/every-statement.sh` writes the tsv and prints the five tables.
- R3 `cargo test --test 18_statement_counts` green three runs; the per-phase
  statement counts and the `vm_step` and `fullscan_step` sums are pinned
  exactly, no runs and no tolerance.
- R4 `cargo test` green, same set as `origin/main`.
- R5 `cargo clippy --lib -- -D warnings` clean. `--all-targets` fails on two
  pre-existing lints in `tests/6_extension_load.rs` (an undeclared `bench` cfg
  and a needless borrow); both fail on `origin/main` too.
- R6 the section above carries a verdict on every row.
- R7 `git diff --stat origin/main...HEAD` lists `src/`,
  `tests/18_statement_counts.rs`, `plans/costs/`, `scripts/every-statement.sh`,
  `Cargo.toml`, `Cargo.lock`, `docs/failure-modes.md`.
- R8 no bounded-loop scanner exists in this tree; the budgets above are the
  bound, and `tests/13_statements_per_drain.rs` and this file keep the loops
  honest.

Every loop the statements code touches is bounded by a named constant:
`SEED_STATEMENT_BUDGET` (one million seeded rows, protects the per-row seed),
`BULK_ROUND_BUDGET` (fixpoint closures), `BULK_GROUP_BUDGET` (group keys),
`BULK_MULTIPLICITY_BUDGET` (bag expansion). The wall pass runs three times and
none of the five scenarios reaches ten seconds; the largest median is 9.5 ms.
