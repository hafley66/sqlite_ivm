---
created: 2026-09-19
updated: 2026-09-21
type: improvement
status: done
priority: high
epic: ivm-correctness-and-storage
labels: [storage]
closed: 2026-09-21
commits:
- hash: 7551b9e
  summary: intern arrangement keys and measure the TEXT baseline
- hash: 3cee12b
  summary: intern result state keys
- hash: c86d76f
  summary: intern arrangement delta identities
- hash: 69ec8cc
  summary: intern fixpoint deletion keys
---

# Arrangement keys are JSON text on every index, against the relational law

## Description

DDL at `src/1a_relational.rs:268`:

```sql
CREATE TABLE "v_op417_1"(
  __k TEXT NOT NULL,          -- json_array string, group and join key
  __r TEXT NOT NULL UNIQUE,   -- json_array string, row identity
  __n INTEGER NOT NULL,
  c0, c1, c2                  -- real columns, typeless
);
CREATE INDEX ON "v_op417_1"(__k);
```

Columns are real columns, so rows are not stuffed into cells. The keys are the
problem. Every b-tree entry keys on a variable-length TEXT value instead of an
8-byte integer, on two indexes (`__k` explicit, `__r` via UNIQUE), on every
operator table, on every write. `_state` at `:251` repeats the shape with
`__key TEXT`.

The same composite is serialized twice per row and both copies are indexed.

Violates `.claude/skills/sql-relational-design`: surrogate INTEGER keys,
natural TEXT keys once in a dictionary table. Read `.claude/skills/sqlite-costs`
before proposing a replacement.

Why it is that way: a key is a composite of N mixed-type values and SQLite has
no tuple type. Serializing to one TEXT column is the first thing that works.

## Decision: intern the composite (user, 2026-09-20)

A dictionary table maps a surrogate INTEGER key to the composite. Arrangement
tables and every index key on that integer. Queries that need the composite
back join the dictionary; queries that need to search inside it get an index on
the dictionary, not a scan.

| option | verdict |
|---|---|
| intern in a dictionary table, key by rowid | **chosen**. One lookup per key, integer b-trees everywhere else |
| N separate key columns plus a composite index | rejected. Arity is per-node, the DDL stops being uniform |
| hash to INTEGER | rejected. Collisions need a tiebreak column |
| keep TEXT, as today | the measured baseline |

Measure each step as it lands. No batched rewrite followed by one measurement.

## Acceptance Criteria

- [x] a dictionary table with a surrogate INTEGER key, indexed for the lookups
      the engine actually issues
- [x] `__k` and `__r` on every arrangement table become INTEGER
- [x] b-tree write rates measured per step against the TEXT baseline, using
      `.claude/skills/sqlite-costs`
- [x] if changed, the frozen goldens stay byte-identical
