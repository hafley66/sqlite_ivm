# Write amplification and duplication across IVM engines

## The question

For the same circuit and the same input, how much state does each engine keep,
and how many times does a single source value appear in it?

sqlite_ivm's own numbers, measured 2026-09-20 on a three-operator view over two
2,000-row integer tables:

| | rows | note |
|---|---|---|
| source | 4,000 | `a` and `b`, 3 columns each |
| `chain_op4_0` | 2,000 | verbatim copy of `a` |
| `chain_op4_1` | 2,000 | verbatim copy of `b` |
| `chain_op6_0` | 80,000 | the join product, 6 columns |
| `chain_state` | 50 | the answer |
| on disk | 8,941,568 bytes | |

21 state rows per source row, and 1,600 state rows per answer row. Every state
row writes its values three times: `c0..cN`, again serialized into `__r TEXT
UNIQUE`, and the key portion again into `__k TEXT` with its own index.

Whether that is bad depends entirely on what the alternatives do. Nobody has
measured that. This study does.

## Non-goals

- Not a speed benchmark. The rig already measures time.
- Not a redesign. The output is numbers and a verdict, not a new engine.
- No conclusion about "the right architecture" before the numbers exist.

## Equal footing, stated as a rule

Each engine gets its own idiomatic spelling of the same circuit. No engine is
asked to accept another's dialect.

| engine | how the circuit is expressed |
|---|---|
| pg_ivm | plain SQL through `pgivm.create_immv`, exactly as its docs show |
| pglite_ivm | same SQL, same call |
| sqlite_ivm | plain SQL through `sqlite_ivm_create` |
| differential dataflow | a dataflow built with `join`, `reduce`, `iterate` |
| DBSP | a circuit built with its own operators |

The circuit catalog is the shared contract, not the SQL text.
`bench/shared/30_circuit_workload.mjs` and `36_semantic_catalog.mjs` already hold
20 named shapes with a JS oracle and a sha256 per expected state, and
`bench/53_shootout.mjs` already drives eight adapters against them. This study
adds an axis to that rig; it does not build a second one.

A shape an engine cannot express is reported as unsupported, with the reason. It
is not worked around with a hand-written equivalent, because a hand-written
equivalent measures the author, not the engine.

## The two metrics

Bytes are not comparable across an on-disk engine and an in-memory one. The
primary metrics are logical and engine-neutral. Bytes are reported per engine as
a secondary, never compared across engines without saying so.

### Write amplification

```
                  rows written into maintenance state
amplification  =  -----------------------------------
                       rows written into sources
```

Counted over the whole run, inserts and deletes both. A retraction is a write.

### Duplication

```
               total occurrences of one source tuple's values across all state
duplication =  --------------------------------------------------------------
                                          1
```

Counted for a chosen probe tuple, by inspection of each engine's state, not by
estimate. For sqlite_ivm today that number is at least 3 per arrangement the
tuple reaches, times the arrangements it reaches.

Both are ratios, so they survive the in-memory versus on-disk gap.

## How each engine's state is read

This is the part that can go wrong quietly. Each entry needs a receipt showing
the number came from the engine, not from arithmetic.

| engine | state introspection | risk |
|---|---|---|
| sqlite_ivm | `dbstat` vtab, per table and per index; row counts per arrangement | none, already exposed |
| pg_ivm | `pg_total_relation_size` on the IMMV and each index; `pg_stat_user_tables.n_tup_ins/upd/del` for writes | the IMMV is one table, so amplification may be near 1; confirm there is no hidden delta relation |
| pglite_ivm | same queries, WASM build | may lack `dbstat`-equivalent detail |
| differential dataflow | arrangement trace sizes via `TraceReader`; batch counts per operator | traces compact in the background, so the number moves; sample at a quiesced frontier |
| DBSP | its own storage/metrics API | **not yet in the rig**; adding it is step 1 |

DBSP absence is the single biggest unknown in this plan. If wiring it costs more
than a day, the study ships with four engines and says so, rather than slipping.

## Steps

Each step commits with its own numbers. No batched work followed by one
measurement at the end.

| step | what | done when |
|---|---|---|
| 1 | Add a DBSP adapter to the shootout, correctness only, no measurement | DBSP passes the same correctness gate as `dd` on the shared circuits, or is declared unsupported per shape |
| 2 | Add state-size introspection per engine, behind one interface | each adapter reports `{state_rows, state_bytes, source_rows_written}` at a quiesced point, with a receipt |
| 3 | Measure amplification across the 20 shapes, all engines | a table, one row per shape per engine |
| 4 | Measure duplication for one probe tuple per shape | a table, and for each engine a sentence saying where the copies live |
| 5 | Cross the value-domain axis from #12 | whether `text_nocase` and `mixed_int_real` change any engine's amplification |
| 6 | Write the verdict | `HYPOTHESIS.md` answering: is sqlite_ivm's 21x an outlier, typical, or good |

Steps 3 through 5 are cheap once 1 and 2 exist. The cost is all in 1 and 2.

## What would change our mind

State the falsifiers before the numbers arrive, so the verdict cannot be fitted
to them afterwards.

| finding | what it implies |
|---|---|
| pg_ivm amplification near 1 | materializing only the answer is viable; sqlite_ivm's per-operator arrangements are a choice, not a requirement, and the join-product buffer is the thing to attack |
| dd and DBSP also 10x or more | per-operator arrangements are the price of incremental maintenance; sqlite_ivm is normal and the work belongs in the constant factor, meaning the three-copies-per-row issue |
| dd low, DBSP low, pg_ivm high | the dataflow engines know something the SQL ones do not; find out what, and whether it survives a single-writer SQLite transaction |
| sqlite_ivm alone is high | a defect, not a design property |

## Prior results this must not contradict

- `labs/20260920.1.*`: 96.9 percent of sqlite_ivm's fold clock is SQLite work,
  not Rust. Three steps carry 91 percent: validity, overflow, upsert.
- `labs/20260920.2.*`: batching at `xSync` holds the maintenance count at 1 per
  transaction, but only if savepoints mark rather than flush.
- `probes/2026-09-20-intern-keys/RESULTS.md`: the fixpoint member table writes
  26 MB against roughly 1 MB for the other shapes, at 1,757 rows/s against
  12,000 to 24,000.
- pg_ivm 1.15 has a known wrong answer on `FULL JOIN USING`
  (`bench/43_pg_full_using.sql`). A correctness difference is not an
  amplification difference; keep them in separate columns.

## Open questions for Chris

1. Is DBSP in scope if it costs a day to wire, or does the study ship with four
   engines?
2. Does "duplication" include the source tables themselves, or only maintenance
   state? The ratio changes meaningfully.
3. Is a WASM pglite number worth collecting, or is native Postgres the only
   Postgres that counts here?

## Test plan

**What breaks if wrong:** a number that flatters sqlite_ivm because another
engine's state was not fully counted. Every engine needs a receipt showing what
was included, and an explicit list of what was excluded.

| case | input | expected | why it exists |
|---|---|---|---|
| empty circuit | zero source rows | every engine reports zero state rows | catches an introspection path that reports a constant |
| one row, one operator | 1 insert, `pipeline` | amplification is small and equal across engines | a floor everyone should agree on |
| known blowup | `join` at fanout 40 | sqlite_ivm reports the join product; others report whatever they keep | the case the study exists for |
| insert then delete to empty | N inserts, N deletes | state returns to the empty-circuit number | catches state that grows and never compacts |
| repeated identical inserts | same row N times | multiplicity counter, not N stored rows | catches an engine that stores duplicates it should count |

**Untested and why:** wall time, already covered by the shootout. Concurrency,
SQLite is single-writer.

**The trap:** comparing dd's in-memory arrangement bytes against Postgres's
on-disk relation bytes and calling one smaller. The ratio metrics exist to avoid
this; any byte number printed in the final table carries its storage medium in
the same cell.
