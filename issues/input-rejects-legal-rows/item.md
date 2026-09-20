---
created: 2026-09-19
updated: 2026-09-19
type: bug
status: open
priority: normal
epic: ivm-correctness-and-storage
labels: [correctness, todo]
---

# input() rejects rows SQLite itself stores

## Description

`src/1a_relational.rs:393-405`. SQLite stores `1.5` in an INTEGER column,
`'abc'` in a numeric column and `x'ff'` in a typed column. The affinity
validator refuses all three.

Consequence: creating a view over a table already holding such a value fails
in `populate` at `:365`, and after creation the user's own INSERT or UPDATE
aborts with `source value does not conform to declared affinity`. This changes
the legality of writes to a table the user owns.

The restriction exists because `CAST(c AS INTEGER)` at `src/0b_relational.rs:220`
would alter 1.5 to 1.

## Fix shape

Declare shadow columns with the source's affinity keyword so storage applies
the same coercion the source did, drop the CAST for column references, and
keep CAST only for `Role::Params` rows at `:155-166` where there is no column
to carry affinity.

## Acceptance Criteria

- [ ] a view over a table holding a real in an INTEGER column creates and
      maintains correctly
- [ ] the affinity check is gone or narrowed to cases SQLite itself rejects
