---
created: 2026-09-19
updated: 2026-09-20
type: bug
status: closed
priority: high
epic: ivm-correctness-and-storage
labels: [perf, reproduced]
---

# Group LIMIT unrolls every multiplicity copy before the LIMIT applies

## Description

`src/1a_relational.rs:688-702`. The `expanded` recursive CTE seeds `__copies`
from `__n` and counts down to 1, emitting one row per copy. `EXPLAIN QUERY
PLAN` shows `USE TEMP B-TREE FOR ORDER BY` after the recursion, so the outer
`LIMIT` cannot stop it.

The `candidates` CTE does cap distinct rows at `limit+offset`. The comment at
`:686` is right about rows and wrong about copies: cost is the sum of `__n`
over the candidates.

## Measured, one group, LIMIT 3, three rows returned every time

| `__n` | run time |
|---|---|
| 1,000 | 0.000215 s |
| 10,000 | 0.001629 s |
| 100,000 | 0.016379 s |
| 1,000,000 | 0.194004 s |

Linear in `__n`. Paid again on every change to that group.

## Fix, tested

Seed `min(__n, limit+offset)` instead of `__n`. Measured 0.157 s to 0.000101 s
on the one-million case, output identical. Differential test: 8 multiplicity
shapes x LIMIT 1 to 5 x OFFSET 0 to 3, 160 cases, 0 mismatches.

Correct because at most `limit+offset` bag rows are ever selected, so no
candidate can usefully supply more copies than that. It degrades gracefully:
a large OFFSET makes the cap large and the answer stays right.

The `window` branch genuinely needs every copy. Document it as O(sum of `__n`)
rather than capping it.

## Acceptance Criteria

- [x] the `limit` branch seeds `min(__n, limit+offset)`
- [x] the `window` branch carries its complexity in a commit message
- [ ] a growth assertion pins Group entry count as Constant in input
      multiplicity (blocked: `assert_growth` landed in hafley-observe after the
      `0.1` this crate pins at `Cargo.toml:23`)
