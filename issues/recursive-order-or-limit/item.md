---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# recursive ordering and LIMIT unsupported

## Description

## Description

Throw site `src/0b_relational.rs:1564`: `recursive ordering and LIMIT unsupported`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
WITH RECURSIVE r(n) AS (SELECT 1 UNION SELECT n+1 FROM r LIMIT 10) SELECT * FROM r
```

## Acceptance Criteria

- [ ] fixture `recursive_limit` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
