---
created: 2026-09-19
updated: 2026-09-20
type: task
status: closed
priority: high
epic: lab-queue-round-one
blocked_by: ['@lab-wide-table-harness']
labels: [lab]
collision: [labs/*/src/**]
lane: lab-rig
lane_seq: 20
size: M
---

# Lab 1: the gang finds out where the time went

## Description

Where does the time actually go. This decides whether labs 3 through 9 are worth
starting at all.

Per single source-row change, at M=20, today's work is nine steps, every one of them
on the row clock. Sites are in `docs/2026-09-19-vtab-clocks.md` under "What is nailed
to which clock today".

## Measure

Span every one of the nine steps. Report percent of wall time per step at M=20.
Run the same fold with the subscriber on and off and report the delta, because the
default filter is `trace` and the logger may be the thing being measured.

## The prior that must be tested, not assumed

A previous measurement on a different engine fold put SQLite at 11.9% of wall time
and application-side Rust at 88%. If that holds here, the SQL-side labs are noise
and the work belongs elsewhere.

## Acceptance Criteria

- [x] percent of wall time for each of the nine steps at M=20
- [x] subscriber on vs off delta, same fold, same seed
- [x] a ranking of labs 3 through 9 by measured upside, not guessed
- [x] runs on the lab 0 rig unmodified

## Test Plan

**What breaks if wrong:** labs 3 through 9 get ranked by a profile that measured the
profiler. The nine steps are on the row clock; if the subscriber is also on the row
clock, the attribution is circular.

**Units under test:** the attribution itself. This lab's output is a table, so the
test is that the table is stable and sums correctly.

```rust
use oh::test;

#[test(timeout = "10s")]
fn nine_steps_sum_to_total_wall_time() { ... }

#[test(timeout = "10s")]
fn attribution_is_stable_across_runs() { ... }

#[test(timeout = "10s")]
fn subscriber_overhead_is_measured_not_assumed() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| sums | one fold at M=20 | per-step percents sum within tolerance of 100 | a step measured twice or not at all shows up here and nowhere else |
| stable | same seed, three runs | ranking unchanged | a ranking that flips between runs cannot schedule anything |
| observer cost | subscriber on vs off | the delta is reported, not assumed zero | the default filter is `trace`; this is the one number that can invalidate the whole profile |
| shape holds | M=10 vs M=20 | per-column steps scale with M, others flat | separates the per-column costs from the fixed ones, which is what ranks labs 3 and 4 |

**Untested and why:** absolute timings. They are machine-specific and asserting on
them makes the suite fail on a different laptop. Ratios and rankings are the durable
output; the numbers go in the lab's verdict, not in an assertion.

**The result that kills this arc:** if SQL-side steps are a small share and Rust-side
re-scanning dominates, labs 3 through 9 are noise. That outcome is a success for this
lab, not a failure.
