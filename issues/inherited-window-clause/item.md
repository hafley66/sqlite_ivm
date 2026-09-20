---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# inherited windows unsupported

## Description

## Description

Throw site `src/0b_relational.rs:1206`: `inherited windows unsupported`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
SELECT rank() OVER w2 FROM a WINDOW w1 AS (PARTITION BY k), w2 AS (w1 ORDER BY v)
```

## Acceptance Criteria

- [ ] fixture `window_inherit` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
