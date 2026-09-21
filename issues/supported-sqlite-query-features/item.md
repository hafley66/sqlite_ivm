---
created: 2026-09-20
updated: 2026-09-20
type: epic
status: open
priority: normal
labels: [planner]
---

# Supported SQLite query features

## Description

## Description

One issue per planner throw site in `src/0b_relational.rs`. Each carries the
line, the literal SQL that hits it, and whether it is a user decision or not
built yet. The built surface, each with a pg-oracle fixture in
`tests/fixtures/1_features.json`:

| clause | fixtures |
|---|---|
| inner, left, right, full, cross, comma, USING, NATURAL, theta, self, chained joins | `right_join` `full_join` `theta_join` `cross_join` `outer_chain` `self_outer` `using_star` |
| WHERE, correlated EXISTS, NOT EXISTS | `filtered_exists` `joined_exists` |
| GROUP BY, HAVING, FILTER, DISTINCT aggregates, ordinals | `group_only` `having` `having_alias` `aggregate_filter` `distinct_aggs` |
| ORDER BY with LIMIT and OFFSET | `distinct_topk` `group_topk` `compound_topk` `bag_topk` `hidden_order` |
| DISTINCT, UNION, UNION ALL, INTERSECT, EXCEPT | `nested_distinct` `compound_topk` |
| CTE, recursive CTE with one recursive reference | `cte_shared` `closure_binary` `nested_fixpoints` `two_step_rules` |
| 16 window functions, frames, named windows, FILTER | `window_multi` `window_frame` `window_named` `window_topk` |
| subquery in FROM at any depth | `outer_aggregate` `group_window` |
| CASE, CAST, LIKE, persistent scalar builtins | `case_cast` `like` |

ORDER BY without LIMIT is accepted and ignored: a view is a bag.

## Acceptance Criteria

- [ ] every throw site in `src/0b_relational.rs` has a child issue or a row in `AGENTS.md` saying it is a decision
- [ ] each child issue names its line, its literal SQL, and a fixture name it would add
