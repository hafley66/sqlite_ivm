---
created: 2026-09-19
updated: 2026-09-20
type: task
status: closed
priority: normal
epic: lab-queue-round-one
labels: [lab]
lane: lab-probe
lane_seq: 10
size: S
---

# Lab 2: the gang compiles a flag they never needed

## Description

A build-flag gate. Cheap, disjoint from the harness work, and a negative result
kills a whole later arc before anyone writes it.

## Questions

- Does `rusqlite` build with the `session` feature on this toolchain
- Does `SQLITE_ENABLE_SESSION` require `SQLITE_ENABLE_PREUPDATE_HOOK` too
- Does a session record a table with no declared PRIMARY KEY
- Does a session see writes that went through a virtual table's `xUpdate`

## Why it matters

The session extension is the only native mechanism in SQLite that coalesces many row
changes into one pull-based diff. If it works here it replaces the entire generated
trigger fleet at `src/1_maintenance.rs:366-405`. If it does not see vtab writes or
PK-less tables, that arc is dead and nobody spends a week finding out.

Changeset, not patchset: a patchset strips old non-PK values, and retraction needs them.

## Acceptance Criteria

- [x] yes or no on each of the four questions, with the probe that answered it
- [x] if yes, the smallest program that attaches a session and drains a changeset
- [x] ISO lab, no path dep on the entry point

## Test Plan

**What breaks if wrong:** a whole later arc gets written against a mechanism that
cannot see this repo's data. The four questions are gates, and a wrong yes is
expensive in a way a wrong no is not.

**Units under test:** SQLite's session extension against this repo's actual table
shapes. No engine code is touched.

```rust
use oh::test;

#[test(timeout = "2s")]
fn session_feature_compiles() { ... }

#[test(timeout = "2s")]
fn session_records_a_table_without_a_declared_primary_key() { ... }

#[test(timeout = "2s")]
fn session_sees_writes_that_went_through_xupdate() { ... }

#[test(timeout = "3s")]
fn changeset_coalesces_three_statements_into_one_entry() { ... }

#[test(timeout = "2s")]
fn changeset_carries_old_values_and_patchset_does_not() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| builds | `rusqlite` with `session` | compiles, and the preupdate flag requirement is recorded | the cheapest possible no |
| no PK | a table with no declared PRIMARY KEY | recorded or not, either answer is the finding | this repo has such tables; a silent skip would be the worst outcome |
| vtab writes | an insert routed through `xUpdate` | recorded or not | if no, lab 8 is dead, because every maintenance write goes through the vtab |
| coalescing | insert, update, insert+delete | one entry out | the claimed advantage over per-row hooks; unverified until measured |
| old values | changeset vs patchset | changeset keeps them, patchset strips them | retraction needs old values, so this decides which form lab 8 can use |

**Untested and why:** performance. This lab is a feasibility gate. If every gate
passes, lab 8 measures; if any fails, there is nothing to measure.

**A failing gate is a passing lab.** The verdict is an answer, not a green suite.
