---
created: 2026-09-19
updated: 2026-09-19
type: bug
status: open
priority: high
epic: ivm-correctness-and-storage
labels: [security]
---

# SQLITE_VTAB_INNOCUOUS never declared, forcing trusted_schema=ON globally

## Description

`src/2_vtab.rs:15-24`. No `sqlite3_vtab_config` call anywhere in the crate.

Consequence: every application embedding this must run `trusted_schema=ON` on
the whole connection, which reopens the schema-injection class that flag
exists to close. The `pragma_recursive_triggers` guard in the trigger body has
the same dependency: with `trusted_schema=OFF` it fails with `unsafe use of
virtual table "pragma_recursive_triggers"`.

`SQLITE_VTAB_CONSTRAINT_SUPPORT` is correctly absent: xUpdate is write-only
through hidden columns and rejects everything else at `:379-387`.

## Acceptance Criteria

- [ ] `sqlite3_vtab_config(db, SQLITE_VTAB_INNOCUOUS)` in xConnect and xCreate
- [ ] the pragma guard replaced with something legal in an untrusted schema,
      for example an extension-registered `SQLITE_INNOCUOUS` scalar reading
      the pragma from C
- [ ] a test runs the whole battery with `trusted_schema=OFF`
