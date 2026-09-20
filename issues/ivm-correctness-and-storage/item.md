---
created: 2026-09-19
updated: 2026-09-19
type: epic
owner: hafley66
status: open
priority: high
---

# sqlite_ivm hardening: correctness, durability, storage layout

## Description

Five reviews on 2026-09-19 read the engine from different angles. Verdicts:
architecture is sound but mis-scoped; the SQL is written by someone who knows
SQLite; no corruption class found. Two defects are reproduced, one storage
layout violates this repo's own relational law, and one perf class is proven
by measurement.

The engine is counting incremental view maintenance, not differential
dataflow. One arrangement table per operator input carrying `__k` group key,
`__r` exact identity and `__n` multiplicity. Every operator follows one
invariant: emit the row stored in the arrangement. Exactly one operator
breaks it.

Children of this epic are ordered by severity, not by effort.
