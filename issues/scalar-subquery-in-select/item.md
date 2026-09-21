---
created: 2026-09-20
updated: 2026-09-20
type: feature
status: open
priority: normal
epic: supported-sqlite-query-features
labels: [planner]
---

# unsupported expression: scalar subquery in SELECT list

## Description

## Description

Throw site `src/0b_relational.rs:439`: `unsupported expression`. A correlated
scalar subquery in the SELECT list. Rewritable today as a LEFT JOIN to a
grouped CTE plus `coalesce`, which is what the planner would emit.

```sql
SELECT s.customer_id, (SELECT count(*) FROM downstream d WHERE d.root = s.customer_id) AS reach FROM spend s
```

## Acceptance Criteria

- [ ] fixture `scalar_subquery_select` in `tests/fixtures/1_features.json` with a pg oracle
- [ ] the planner lowers it to Group plus left Join, no throw
