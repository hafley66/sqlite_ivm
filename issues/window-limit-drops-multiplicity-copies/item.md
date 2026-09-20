---
created: 2026-09-20
updated: 2026-09-20
type: bug
status: open
priority: high
epic: ivm-correctness-and-storage
labels: [correctness, reproduced]
---

# A window with LIMIT drops a multiplicity copy

## Description

Reproduced on `43d695e`, before and after the Group LIMIT clamp, so the defect
predates both. Fail-pre-fix test in tree:
`tests/8_group_limit.rs::window_with_limit_reads_every_copy`, ignored under
`@window-limit-drops-multiplicity-copies`.

## What happens

Shape `[3,1,1]`: `v=0` three times, `v=1` once, `v=2` once. Five bag rows.

```sql
SELECT v AS value, SUM(v) OVER() AS total FROM a LIMIT 4
```

| path | rows returned |
|---|---|
| plain SQL | `0,0,0,1` |
| the view | `0,0,1,2` |

The view drops one copy of `v=0` and pulls `v=2` forward to fill the LIMIT. The
`total` column is 3 on both sides, so the window aggregate itself is right; the
bag feeding the outer LIMIT is short one copy.

`ROW_NUMBER() OVER(ORDER BY v,id)` under the same LIMIT agrees with plain SQL,
which is why the existing battery stayed green. Only an aggregate over the whole
bag exposes it, and only when a value carries multiplicity above one.

## Where to look

`src/1a_relational.rs`, the `Kind::Group` branch taken when `*window ||
limit.is_some()`. Two sites build the same `expanded` recursive CTE, the
incremental one and the bulk one. The `window` case keeps the unclamped `__n`
seed by design, so the loss happens elsewhere in that path.

## Acceptance Criteria

- [ ] `window_with_limit_reads_every_copy` passes with the `#[ignore]` removed
- [ ] the three window shapes in that test stay green under all eight
      multiplicity shapes
- [ ] a `docs/failure-modes.md` row: incident, cause, the fail-pre-fix test,
      the rail that keeps it from returning
