---
created: 2026-09-19
updated: 2026-09-20
type: task
status: closed
priority: high
epic: lab-queue-round-one
labels: [lab]
collision: [labs/*/src/**]
lane: lab-rig
lane_seq: 10
size: M
---

# Lab 0: the gang builds a wider table

## Description

Every measurement in later labs is read against this rig, so it lands first.

Fixed shape across all labs: N source tables, M columns each, J-way joins.
M at 10 and 20, not 4. N at 2, 4, 8. J up to 4. Fixed seed.

## Why it comes first

Lab 1 profiles against it. Labs 3 through 9 read their wins against lab 1's
profile. A rig built per-lane means every lane builds its own ruler.

## Emit spans, not timers

The rig's spans must be assertable by `hafley-observe`'s `CountRecorder`, so every
later lab inherits measurement and none of them re-invent a stopwatch.
`assert_growth` and `assert_children_at_most` are the interface.

## Pin the subscriber

`hafley-observe/src/1_format.rs:33` falls back to `trace` when no filter is set, by
deliberate design for a CLI. In a fold that emits a span per row this makes the
logger the bottleneck. Every lab pins `HAFLEY_LOG` explicitly.

## Acceptance Criteria

- [x] generator produces N x M x J tables from a fixed seed, reproducible
- [x] M covers 10 and 20
- [x] spans named stably so `CountRecorder` can key on them
- [x] `HAFLEY_LOG` pinned, never inherited
- [x] ISO: own workspace, deps pinned from the entry point, no path dep on it

## Test Plan

**What breaks if wrong:** every later lab reads its number off a rig nobody checked,
and a non-reproducible generator makes two labs disagree for reasons neither can find.

**Units under test:** the generator, not the engine. The engine is the subject, the rig
is the instrument, and an uncalibrated instrument is worse than none.

```rust
use oh::test;

#[test]
fn same_seed_same_tables() { ... }

#[test]
fn different_seed_different_tables() { ... }

#[test(cases = [("narrow", 10), ("wide", 20)])]
fn column_count_is_honored(name: &str, m: usize) { ... }

#[test(timeout = "8s")]
fn spans_are_countable_by_the_recorder() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| reproducible | seed 42 twice | byte-identical tables | two labs must be comparable |
| seed matters | seed 42 vs 43 | different tables | a generator ignoring its seed looks reproducible and is useless |
| width honored | M in {10, 20} | that many columns | the whole point is not-4-column tests |
| join arity | J in {2, 3, 4} | that many tables joined | J is the other axis |
| spans countable | one fold | `CountRecorder` keys resolve | later labs assert on these names; a rename breaks them silently |
| filter pinned | `HAFLEY_LOG` unset | the rig sets it anyway | `hafley-observe/src/1_format.rs:33` defaults to `trace`, which makes the logger the bottleneck |

**Untested and why:** the correctness of the fold. That is `tests/*.rs`'s job. This
rig only has to generate and observe.

**Budgets:** memory budget on the generator only, so a rig that itself allocates
gigabytes is caught before it poisons lab 1's numbers.
