---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# unsupported binary operator

## Description

## Description

Throw site `src/0b_relational.rs:297`: `unsupported binary operator`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
SELECT a.v -> '$.x' FROM a
```

## Acceptance Criteria

- [ ] fixture `json_arrow_operator` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
