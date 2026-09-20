---
created: 2026-09-19
updated: 2026-09-19
type: improvement
status: open
priority: normal
epic: ivm-correctness-and-storage
labels: [perf]
---

# Outer, semi and anti joins run one SQL statement per pair, twice

## Description

`src/1a_relational.rs:570-650`. `snapshot()` loads both sides for the key,
then for every (left, right) pair calls `evaluate(db, &out, &["1"], Some(p))`
at `:94-99`, one statement execution each. `before` and `after` each run the
whole thing, so a key with L left and R right rows costs 2 x L x R statement
executions per single source row change.

## The obvious part

For `semi` and `anti` the inner loop keeps running after `matched = true`. It
pushes no result, and `right_matched` is only read by the `right` and `full`
branches, which those modes never take. A `break` turns L x R into L x
(position of first match).

## The rest

Push the predicate into the arrangement read: `SELECT c..,__n FROM other WHERE
__k=?1 AND <predicate with the left row bound>`. One statement per side per
snapshot instead of per pair.

Related, mechanical: `src/1a_relational.rs:104`, `:479` and `:670` use
`Connection::query_row`, which prepares and finalizes every call. Everything
around them uses `prepare_cached`.

## Acceptance Criteria

- [ ] semi and anti break on first match
- [ ] the predicate is evaluated in SQL, not per pair in Rust
- [ ] the three `query_row` calls use `prepare_cached`
- [ ] a growth assertion pins join statement count as Constant in L x R
