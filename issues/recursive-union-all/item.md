---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# recursive UNION ALL unsupported

## Description

## Description

Throw site `src/0b_relational.rs:1576`: `recursive UNION ALL unsupported`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<5) SELECT * FROM r
```

## Acceptance Criteria

- [ ] fixture `recursive_union_all` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
