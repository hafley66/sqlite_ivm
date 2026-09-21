---
created: 2026-09-20
updated: 2026-09-20
type: bug
status: open
priority: high
epic: lab-queue-round-one
labels: [bench]
---

# Circuit adapters ignore the fixture value domain, so the shootout report prints no rows

## Description

`bench/shared/30_circuit_workload.mjs` emits `text_nocase` and `mixed_int_real`
fixtures since #12 (`5d97de4`). None of the native circuit adapters read
`table_schema` or `value_domain`: `examples/4_sqlite_case.rs` declares integer
columns and the extension rejects the rows (`source value does not conform to
declared affinity`), `bench/shared/34_circuit_dd.rs:18` unwraps `as_i64` on a
text value and panics, `bench/shared/32_circuit_postgres.mjs` fails on insert.
Every engine fails 2 of 3 domains, and `bench/51_shootout_report.mjs` drops a
case unless every domain validates, so the quick and full profiles print an
empty performance table. Receipts for the integer domain are complete and
`plans/costs/shootout-quick.md` was read from `logs/performance.log` directly.

## Acceptance Criteria

- [ ] every circuit adapter creates `a`, `b`, `c` from the fixture's `table_schema`
- [ ] `34_circuit_dd.rs` carries text and real values
- [ ] `node bench/53_shootout.mjs quick --engines sqlite-ivm,pg-ivm,sqlite-query,pg-query,dd` prints the performance table
