---
created: 2026-09-20
updated: 2026-09-20
type: task
status: open
priority: high
epic: lab-queue-round-one
lane: lab-engine
lane_seq: 10
labels: [lab]
size: M
collision: [src/1_maintenance.rs, src/2_vtab.rs]
---

# Lab 4: the gang stops talking in json

## Description

Today the trigger serializes the changed row to JSON text and the vtab parses it back.
At M=20 that is 20 encodes in SQL, then `json_valid` + `json_type` + `json_array_length`,
then a `json_each` scan, then 20 `json_extract` calls. Four of the nine row-clock steps
in `docs/2026-09-19-vtab-clocks.md` exist only to undo the first one.

## The change

Declare one hidden column per used source column instead of one `__ivm_row TEXT`.
Declaration site is `src/2_vtab.rs:167`. Trigger body is `src/1_maintenance.rs:399`.
Values arrive in the `xUpdate` argv already typed, so nothing is parsed.

## Why it stands without a profile

The JSON round trip is pure overhead: the trigger already has the typed values and
throws the types away. A hidden column carries them across the same boundary without
the encode. The profile would size the win; it cannot make the design wrong.

## Constraint

The hidden-column count varies per source table, so the declared schema is built from
the plan rather than fixed. That is the one real design decision in this lab.

## Acceptance Criteria

- [ ] one hidden column per used source column, declared from the plan
- [ ] the trigger binds values directly, no `json_array`
- [ ] `json_valid` / `json_type` / `json_array_length` / `json_each` / `json_extract` all gone from the maintenance path
- [ ] the existing battery is green, unchanged
- [ ] a `CountRecorder` assertion pins statements per maintained row, and it drops

## Test Plan

**What breaks if wrong:** the maintenance path silently drops or mistypes a column.
JSON was lossy in a visible way (everything became text); hidden columns are lossy in
an invisible way (affinity coerces and nobody notices).

**No timing runs.** Assertions are on statement counts via `CountRecorder`, which is
deterministic and costs nothing under load.

```rust
use oh::test;

#[test]
fn hidden_columns_match_the_used_column_count() { ... }

#[test(cases = [("narrow", 3), ("wide", 20)])]
fn every_used_column_survives_the_boundary(name: &str, m: usize) { ... }

#[test]
fn integer_stays_integer_across_xupdate() { ... }

#[test]
fn maintenance_runs_no_json_functions() { ... }

#[test]
fn writing_visible_columns_is_still_refused() { ... }
```

| case | input | expected | why it exists |
|---|---|---|---|
| column count | a plan with K used columns | K hidden columns declared | the count is built from the plan, which is this lab's one design decision |
| round trip | M ∈ {3, 20} | every value arrives unchanged | the failure this replaces was lossy; the replacement must not be |
| type preserved | integer source values | integer in `xUpdate`, not text | affinity coercion is silent, and `src/1_maintenance.rs:378` already requires integers |
| no json | one maintained row | zero `json_*` calls on the path | the whole point; a leftover `json_extract` means the arc half-landed |
| still read-only | an insert with visible columns set | refused | `src/2_vtab.rs:389` is the access control, and widening the schema is exactly where it breaks |
| statement count | one maintained row | pinned, and lower than before | the win, measured as a count rather than a clock |

**Untested and why:** wall time. Other agents are on the machine and a timing number
taken under load is worse than none. The count assertion carries the claim.

**Regression bar:** the existing battery green and unchanged. A test edited to
accommodate the new shape is a finding to report, not a thing to do quietly.
