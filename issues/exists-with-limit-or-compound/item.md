---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# EXISTS with LIMIT, compound or WITH unsupported

## Description

## Description

Throw site `src/0b_relational.rs:1073`: `EXISTS with LIMIT, compound or WITH unsupported`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
SELECT * FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.k=a.k LIMIT 1)
```

## Acceptance Criteria

- [ ] fixture `exists_limit` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
