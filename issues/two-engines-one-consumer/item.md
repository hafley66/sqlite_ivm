---
created: 2026-09-19
updated: 2026-09-19
type: chore
status: open
priority: normal
epic: ivm-correctness-and-storage
labels: [scope]
---

# A second integer-only engine doubles the surface for one consumer

## Description

`src/0_query.rs` plus `src/1_maintenance.rs`, 782 lines, is the integer COUNT
and SUM fast path carried over from the pg_ivm benchmark. It has its own
Query, Filter and Column model, its own trigger generator (`src/2_vtab.rs:284`
against `:286`) and its own storage-format branch (`:97-105`, `:185`, `:236`).

The staff review named it as the largest piece of mis-scoping: double the
surface a maintainer must hold, one consumer.

Related scope the same review would refuse at `bind` until a consumer asks:
window functions, LIMIT and OFFSET inside Group, FULL and RIGHT joins, RTRIM
collation. Each re-admitted behind the corpus test in
@key-encoding-agreement-test.

Also here: two `loop {}` at `src/1a_relational.rs:787` and `:839` carry no
budget and no named diagnostic, against this repo's standing law that every
loop and recursion is bounded.

## Acceptance Criteria

- [ ] the integer engine is deleted or routed through Group
- [ ] one storage format
- [ ] both loops carry a budget constant and a named diagnostic
- [ ] refused constructs fail at `bind` with a named error
