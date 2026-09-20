---
created: 2026-09-19
updated: 2026-09-19
type: task
status: open
priority: normal
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

- [ ] yes or no on each of the four questions, with the probe that answered it
- [ ] if yes, the smallest program that attaches a session and drains a changeset
- [ ] ISO lab, no path dep on the entry point
