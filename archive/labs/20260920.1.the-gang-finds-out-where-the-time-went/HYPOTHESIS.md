# the gang finds out where the time went

## Hypothesis

Per-step wall attribution on the rig can rank labs 3 through 9 by measured
upside. The working prior from the 2026-09-19 clocking notes (11.9 percent
SQLite, 88 percent Rust) predicts the row clock is application bookkeeping;
the attribution confirms or kills that.

## Knob

The subscriber stack (counting instrument only, plus the format stack at the
pinned filter, plus the same at trace) and the prepare policy (engine mode,
which re-prepares three maintenance statements per row, versus cached mode,
the reuse ceiling).

## Invariants assumed of the input tables

- the rig's canonical corner: 8 tables, 20 columns, join arity 4, seed 42;
  2048 changes per leg, median of three legs per configuration.
- values 0..=2 keep the column product inside i64; changes target source 0.

## Measurement

```
cd labs/20260920.1.the-gang-finds-out-where-the-time-went && cargo run --release
```

prints the nine-step percent table, the subscriber on/off delta, the trace
fallback delta, the statement-reuse ceiling, the lab ranking, and the
M=10-versus-M=20 shape. `cargo test` proves the steps sum to the fold, the
ranking is stable across runs, the subscriber cost is measured not assumed,
and the shape splits into per-column and fixed steps.

## Invalidates

The ranking feeds labs 3 through 9: a lab whose knob prices at zero here is
dead unless its motivation is something other than write-path time. If the
statement-reuse ceiling came back small, the fold would be irreducible and
every queued lab would matter; the opposite happened.

## Verdict

2026-09-20, release build, M2 Pro, 2048 changes, median of three legs of each
configuration; three independent runs agreed to within a few percent.

Per-step share of the nine-step total (ns per change in parentheses):

| step | share | ns/change |
| --- | --- | --- |
| step/validity | 31.7% | 23 708 |
| step/overflow | 29.8% | 22 280 |
| step/upsert | 29.7% | 22 216 |
| step/build | 3.0% | 2 256 |
| step/dispatch | 2.3% | 1 733 |
| step/guard/pragma | 1.4% | 1 075 |
| step/guard/types | 1.2% | 877 |
| step/refresh | 0.7% | 554 |
| step/validate | 0.1% | 61 |

- Steps cover 93.9 percent of the fold wall, so the table is the fold.
- SQL-side statements (everything but validate+build) are 96.9 percent of the
  step clock. The 88-percent-Rust prior is dead: the row clock is SQLite work
  in the vtab INSERT path, not Rust bookkeeping.
- Subscriber on versus off: +1.09, -0.07, +0.42 percent across runs; median
  +0.42 percent. With the filter pinned to warn, watching the fold is free.
  The pin exists because an unset filter defaults to trace: priced, that
  fallback costs +1.75 to +2.95 percent (median +2.34) today, and it would
  grow with any per-row event the engine ever adds.
- Statement-reuse ceiling: engine 163.2 ms versus cached 40.6..41.4 ms, so
  re-preparing the three maintenance statements costs about 75 percent of the
  whole fold. This dwarfs every queued lab: it is the first lever, and lab 5
  (buffer in xUpdate, flush in xSync) is the design that removes it.
- Ranking by measured share: 5 (100 percent upper bound, the amortization
  argument), 6 and 7 (6.1..6.3 percent, dispatch+refresh+validate+build), 8
  (2.6 percent, the two guards), 3 (1.4 percent, pragma guard), 4 (zero,
  already shipped as storage format 4), 9 (zero here, read path).
- Shape from M=10 to M=20: per-column steps grow 1.33x..1.61x (validity,
  overflow, upsert, build, typeof guard); fixed steps hold at
  0.99x..1.19x (pragma guard, refresh, validate, dispatch). Width multiplies
  the three fat steps, which is exactly what the reuse ceiling also removes.
- Caveats: this fold is a driver-mode replica of the engine's per-row SQL, so
  it excludes vtab C dispatch overhead, trigger firing machinery, and the
  removal arm; absolute nanos are machine-specific and only the ratios are
  claims.

Doc staleness: `docs/2026-09-19-vtab-clocks.md` lists eight sites from the
removed JSON-payload design (storage format 3). Today's fold runs nine steps:
the doc's pragma guard, typeof guard, dispatch, validate, build, validity,
overflow, upsert all survive unchanged in name, `step/refresh` (the engine's
catalog check) was added, and the doc's hidden-JSON steps map onto build +
upsert in storage format 4.
