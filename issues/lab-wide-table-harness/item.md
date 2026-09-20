---
created: 2026-09-19
updated: 2026-09-19
type: task
status: open
priority: high
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

- [ ] generator produces N x M x J tables from a fixed seed, reproducible
- [ ] M covers 10 and 20
- [ ] spans named stably so `CountRecorder` can key on them
- [ ] `HAFLEY_LOG` pinned, never inherited
- [ ] ISO: own workspace, deps pinned from the entry point, no path dep on it
