---
created: 2026-09-19
updated: 2026-09-19
type: task
status: open
priority: high
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

- [ ] percent of wall time for each of the nine steps at M=20
- [ ] subscriber on vs off delta, same fold, same seed
- [ ] a ranking of labs 3 through 9 by measured upside, not guessed
- [ ] runs on the lab 0 rig unmodified
