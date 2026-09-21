# the gang builds a wider table

## Hypothesis

A seed-stable generator plus a fold driver can exercise the widest corner of
the maintenance path (many source tables, many columns per table, multi-way
join) in well under ten seconds per operation, with every engine step
observable through one counting instrument.

## Knob

Generator axes: source tables N in {2, 4, 8}, columns per table M in
{10, 20}, join arity J in {2, 3, 4}, plus the seed that pins the data.
Changes per fold up to 4096.

## Invariants assumed of the input tables

- values are 0..=2 so the M-column product stays inside i64 and the overflow
  step stays a check, not a crash.
- every fold change targets source 0, the M-wide image the view sums over.
- the same seed always yields the same tables, so any two folds compare.

## Measurement

```
cd labs/20260920.0.the-gang-builds-a-wider-table && cargo run -- 42
```

prints the fold wall plus instances/entries/nanos per span from the counting
instrument. `cargo test` proves generator determinism, axis budgeting, and
that spans land in the recorder.

## Invalidates

Nothing on its own; labs 3 through 9 all consume this harness, so a harness
failure would invalidate every queued lab.

## Verdict

2026-09-20, release build, M2 Pro, seed 42, 512 changes, axes 8/20/4:

- fold wall 73.5 ms in a debug run of the same shape and 512 changes; release
  runs of the 2048-change profile (lab 1) fold in ~163 ms, so one fold is
  roughly two orders of magnitude under the ten-second law.
- `cargo test`: 7 passed (same_seed_same_tables,
  different_seed_different_tables, column_count_is_honored,
  join_arity_is_honored, spans_are_countable_by_the_recorder,
  filter_is_pinned_never_inherited, axis_budgets_name_their_ceiling).
- The generator honors every axis; the recorder sees every span; the log
  filter is pinned by the rig itself.

Instrument sourcing, build versus buy:

| candidate | counts | wall timings | filter pin | status |
| --- | --- | --- | --- | --- |
| hafley-observe 0.1.0 (registry `hafley`, what the lock resolves) | no | no | no | ships format/init plumbing only |
| hafley-observe `feat/observe-sqlite` (unpublished worktree of another lane) | CountRecorder, SpanCounts, Growth asserts | no | no | unpublished; importing it would couple this lane to a moving tree |
| in-lab `1_observe.rs` (chosen) | CountRecorder, SpanCounts, Growth asserts | Timings (nanos per span name) | pin_log_filter, unconditional | shipped with the rig; later labs inherit it by path |

The rig mirrors the exact published API surface of 0.1.0 for the pieces it
reuses (format_layer, FormatConfig, OutputFormat) and adds the two things the
published crate lacks: span counts and span wall clocks. If the counts work
lands upstream, the swap is a one-line dependency change per lab; nothing in
the rig calls private detail.
