---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# recursive step requires inner join

## Description

## Description

Throw site `src/0b_relational.rs:1661`: `recursive step requires inner join`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
WITH RECURSIVE r(x) AS (SELECT 1 UNION SELECT e.y FROM r LEFT JOIN e ON e.x=r.x) SELECT * FROM r
```

## Acceptance Criteria

- [ ] fixture `recursive_left_step` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
