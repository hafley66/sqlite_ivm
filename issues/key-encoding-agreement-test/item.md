---
created: 2026-09-19
updated: 2026-09-19
type: task
status: closed
priority: high
epic: ivm-correctness-and-storage
labels: [correctness]
---

# No test proves the Rust and SQL key encoders agree

## Description

Three encoders exist for the same concept, flagged independently by two
reviewers.

| encoder | where | reals as |
|---|---|---|
| Rust `key()` | `src/1a_relational.rs:35` | folded to integer when integral |
| Rust `identity()` | `src/1a_relational.rs:52` | `json_object('real', <hex bits>)` |
| SQL `key_sql()` | `src/0b_relational.rs:177` | `json_object('real', printf('%!.17g'))` |

The Rust pair feeds Set, Join, Group and the fixpoint input side. `key_sql`
feeds only `all` and `work`. They never cross-compare today, so there is no
bug today. Nothing states that, and one future join between those tables is a
silent wrong answer with no error.

A manual audit on 2026-09-19 compared the bulk SQL path and the incremental
Rust path across integers, integral and non-integral reals, negative zero,
NaN, infinities, the `i64::MAX` boundary, integers beyond f64 exact range,
empty and null-byte blobs, text that mimics the tagged blob encoding, and
NULL. They agreed on every class. That audit is not a test and will not stay
true.

One open PLAUSIBLE case from that audit: a Group key expression returning
JSON-subtyped text is embedded as JSON by the bulk path and quoted as a
string by the incremental path, so `json_extract(j,'$.t')` over
`'{"t":["a"]}'` yields `[["a"]]` versus `["[\"a\"]"]`. Unverified because it
depends on whether the planner admits expression group keys.

## Acceptance Criteria

- [ ] a property test over a value corpus spanning every affinity and
      collation asserts Rust `key()` after `key_expression` equals SQL
      `key_sql()`, and `identity()` round-trips
- [ ] the JSON-subtype group key case is confirmed or rejected
- [ ] which encoder keys which table is written down
