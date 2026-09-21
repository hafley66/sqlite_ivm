# Lane review-amplification

Review, then write one airtight lab spec as an issuectl issue. No engine edits.

Base: `origin/main`. Branch `review/amplification`.

## The finding you are reviewing

Measured 2026-09-20 on a three-operator view over two 2,000-row integer tables:

```sql
CREATE TABLE a(id INTEGER PRIMARY KEY, k INTEGER, v INTEGER);
CREATE TABLE b(id INTEGER PRIMARY KEY, k INTEGER, w INTEGER);
SELECT sqlite_ivm_create('chain',
  'SELECT a.k AS k, SUM(b.w) AS total FROM a JOIN b ON a.k=b.k WHERE a.v>0 GROUP BY a.k');
-- 2000 rows into each, k = i % 50
```

State that results:

| table | rows | columns |
|---|---|---|
| `chain_op4_0` | 2,000 | `__k`, `__r`, `__n`, c0..c2 |
| `chain_op4_1` | 2,000 | `__k`, `__r`, `__n`, c0..c2 |
| `chain_op6_0` | 80,000 | `__k`, `__r`, `__n`, c0..c5 |
| `chain_state` | 50 | `__key`, c0, c1 |

4,000 source rows become 84,000 state rows in 8,941,568 bytes.

One source row `a.id=1` was counted across all state:

| where | copies |
|---|---|
| source table `a` | 1 |
| `op4_0` c-columns | 1 |
| `op4_0` `__r` | 1 |
| `op6_0` c-columns | 40 |
| `op6_0` `__r` | 40 |
| **total** | **82** |

The 40 is join fanout: `a.id=1` matches 40 rows of `b`, and each pair
materializes its values twice, once in columns and once serialized in `__r`.

## Your first job: is 82 right

Re-measure it yourself. Do not take the number on trust. Vary fanout and
confirm the copy count tracks it. Say plainly whether the figure holds, and if
it does not, what the correct one is.

## Your second job: read what the other engines do

Read the actual sources, not blog posts. Answer for each:

**How many times does one source row's content get stored, and where.**

| engine | where to read |
|---|---|
| differential dataflow | `differential-dataflow` crate: `trace/implementations`, `operators/arrange`. The rig already links it at `bench/41_feature_dd.rs` and `bench/49_dd_contracts.rs` |
| DBSP | the `dbsp` crate: its `trace`, `operator/join`, `operator/aggregate`. Not in this repo yet |
| pg_ivm | `pg_ivm` C source: how `create_immv` stores an IMMV and whether any delta relation is kept between statements |

For each, answer these four, each with a file and line:

1. Is a join's output materialized, or recomputed from the two input arrangements?
2. Is each input side stored whole, or as a reference into the source?
3. Is there a per-row identity stored alongside the row, like `__r`?
4. Is the row's content stored once or more than once per arrangement?

Cite the file and line for every answer. An answer you cannot cite is written
as "could not determine, looked at X" rather than guessed.

## Your third job: the issue

File one issue with `issuectl` that a lane could execute without asking a single
question. It measures write amplification and content duplication across
sqlite_ivm, differential dataflow, DBSP and pg_ivm on this repo's own circuits.

Constraints on that spec:

- Each engine gets its own idiomatic spelling of the circuit. pg_ivm gets plain
  SQL through `pgivm.create_immv`. No engine is asked to accept another's dialect.
- The shared circuit catalog is `bench/shared/30_circuit_workload.mjs` and
  `36_semantic_catalog.mjs`, 20 shapes with a JS oracle and a sha256 per state.
  `bench/53_shootout.mjs` already drives eight adapters. Extend that rig; do not
  build a second one.
- The headline metric is the copy count of one probe row, the number this review
  is about. Bytes are secondary and per-engine.
- A shape an engine cannot express is reported unsupported with a reason.
- DBSP is not wired yet. The spec says whether that is in scope and what it costs.

The existing draft is `plans/2026-09-20-write-amplification-study.plan.md`. It is
a draft and it is wordy. Take what is useful, cut the rest, and let your issue
replace it.

## Files you own

```
issues/<the new slug>/item.md                   (new, via issuectl)
plans/2026-09-20-write-amplification-study.plan.md   (may delete or rewrite)
probes/2026-09-20-row-amplification/**          (new, your re-measurement)
```

## Files you must not touch

```
src/**   tests/**   bench/**   labs/**   docs/**   scripts/**   Cargo.toml
issues/*/  (any other issue)
```

## Style laws, inline

- No em dashes. No negative parallelism ("not X, Y"). No rhetorical closes.
- Banned: provenance, substrate, load-bearing, regime, "ground truth" (say oracle),
  "support" as a noun (say refCount).
- Every claim carries a `path:line`, a fixture, or a command that prints it.
- No stray numbers in sentences. Tables for numbers.
- Short sentences. Present tense. No "we".
- **The user has said twice that hedging prose is unacceptable. Write the finding,
  not your reasoning about the finding. If a sentence does not carry a number, a
  path, or an instruction, delete it.**

## Report back

`boop beep --no-wait --as review-amplification sprefa-coordinator "<one line>"`

Commit before reporting done. One line: is 82 correct, and what does dd store
per source row.
