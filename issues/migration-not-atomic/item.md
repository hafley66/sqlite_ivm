---
created: 2026-09-19
updated: 2026-09-19
type: task
status: open
priority: low
epic: ivm-correctness-and-storage
labels: [durability, reproduced, todo]
---

# Storage format migration is not atomic and bricks the view on rerun

## Description

`migrate` at `src/2_vtab.rs:67-107`. Zero occurrences of `SAVEPOINT`, `BEGIN`,
`COMMIT` or `ROLLBACK` in that file.

It runs from `xConnect` during another statement's prepare, in autocommit, so
each step commits separately: drop triggers, create hooks, update
`__ivm_objects`, update `__ivm_schema`.

The format rewrite is correctly last, so format 4 with old triggers cannot
happen. The damage is on rerun: `DROP TRIGGER main."..."` at `:92` has no
`IF EXISTS` and the trigger list still comes from `__ivm_objects`, so the next
connect raises `no such trigger` and `xConnect` fails forever. `DROP TABLE v`
also needs `xConnect`, so the view cannot be removed without hand-editing
`__ivm_views`, `__ivm_schema`, `__ivm_objects` and the shadow tables.

Reachable without a crash: `SQLITE_BUSY` on a rollback-journal database with a
concurrent reader, a `CREATE TRIGGER` error, or two connections migrating at
once with no lock between them.

## Acceptance Criteria

- [ ] `migrate` is wrapped in `SAVEPOINT __ivm_migrate` / `RELEASE` with
      `ROLLBACK TO` on error
- [ ] `DROP TRIGGER IF EXISTS`
- [ ] a test kills the migration between the drop and the create, then proves
      a fresh connect still works and `DROP TABLE` still works
- [ ] concurrent migration from two connections is serialized or proven safe

## Decisions

### 2026-09-19T22:40:33Z · @hafley66

Superseded by POLICY.md. Upgrades rebuild instead of migrating: drop the derived objects and re-derive from __ivm_views.query_sql. There is no old state worth transforming, so the atomicity requirement disappears. Retyped to task, priority low: replace migrate() with a rebuild path. Worst case is a view that needs rebuilding, which is the same operation.
