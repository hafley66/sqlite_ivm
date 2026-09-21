---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# recursive step requires one recursive reference

## Description

## Description

Throw site `src/0b_relational.rs:1681`: `recursive step requires one recursive reference`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
WITH RECURSIVE p(x,y) AS (SELECT x,y FROM e UNION SELECT a.x,b.y FROM p a JOIN p b ON a.y=b.x) SELECT * FROM p
```

## Acceptance Criteria

- [ ] fixture `recursive_self_join` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
