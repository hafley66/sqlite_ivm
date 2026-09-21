---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# unsupported window function

## Description

## Description

Throw site `src/0b_relational.rs:1270`: `unsupported window function`. Not built yet; a compiler
error for an unbuilt construct, not a language limit, unless `AGENTS.md` says
otherwise.

Literal SQL that hits it:

```sql
SELECT group_concat(v) OVER (PARTITION BY k ORDER BY id) FROM a
```

## Acceptance Criteria

- [ ] fixture `window_group_concat` in `tests/fixtures/1_features.json` with a pg oracle, or an `AGENTS.md` row recording the decision to refuse
- [ ] the throw site is gone or its message names the decision
