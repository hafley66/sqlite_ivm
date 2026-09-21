---
created: 2026-09-20
updated: 2026-09-21
type: bug
status: fixed
priority: high
epic: ivm-correctness-and-storage
labels: [correctness, reproduced]
closed: 2026-09-20
closed_by: fable
commits:
- hash: 85dc492
  summary: subtype strip and project_group_input
---

# A JSON-subtype group key splits one group into two rows

## Description

Confirmed by `tests/7_key_agreement.rs:272`, which currently asserts the wrong
answer as a characterization test. Promoted from the PLAUSIBLE finding in the
2026-09-19 key-encoder audit.

Expression group keys are admitted at `src/0b_relational.rs:1339`. Bulk `fill`
preserves the JSON subtype of the key expression; incremental Group maintenance
receives a plain text value at `src/1a_relational.rs:1102`. The two disagree on
what the group key is.

## What happens

```sql
CREATE TABLE json_source(id INTEGER PRIMARY KEY, j TEXT);
INSERT INTO json_source VALUES(1, '{"t":["a"]}');
-- view over:
SELECT json_extract(j,'$.t') AS t, COUNT(*) AS n
FROM json_source GROUP BY json_extract(j,'$.t')
INSERT INTO json_source VALUES(2, '{"t":["a"]}');
```

| path | result |
|---|---|
| plain SQL | one row, `["a"]`, `n = 2` |
| the view | two rows, `["a"]` with `n = 1`, twice |

The first row was keyed by the bulk path, the second by the incremental path.
They do not collide, so the group splits instead of accumulating.

## Note

The key-agreement corpus test passes because it never routes a JSON-subtyped
value through a Group key expression. Rust `key()` and SQL `key_sql()` agree on
every scalar class; the divergence is in what reaches them.

## Acceptance Criteria

- [x] the bulk and incremental paths key an expression group by the same value
- [x] `tests/7_key_agreement.rs:272` asserts one row with `n = 2`, and its name
      stops saying "diverges"
- [x] a `docs/failure-modes.md` row: incident, cause, fail-pre-fix test, rail

## Resolution

### 2026-09-20T19:30:31Z · @fable

||'' strips the JSON subtype in every SQL key encoder; planner project_group_input puts a Map before every non-window Group
