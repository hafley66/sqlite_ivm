---
created: 2026-09-19
updated: 2026-09-19
type: improvement
status: open
priority: normal
epic: ivm-correctness-and-storage
labels: [test]
---

# Bench rig covers 20 shapes but only bounded integers

## Description

`bench/shared/30_circuit_workload.mjs` and `36_semantic_catalog.mjs` hold 20
named shapes with a JS oracle and a sha256 per expected state, driven against
four implementations: sqlite_ivm, pg_ivm, differential dataflow and
SWI-Prolog. Mutations include deletes, and there is a state named
`retract_one_support` at `30_circuit_workload.mjs:67`.

It still missed `fixpoint-emits-rederived-rows`. Line 3 says why: "Fixture
values are bounded integers". The generator emits distinct small integers, so
`__k` and `__r` agree on every row and the two notions of sameness never
diverge. The defect is structurally invisible.

## Fix shape

Add a value-domain parameter to `makeCircuitFixture`, so each shape runs
against integers, then TEXT under NOCASE, then mixed INTEGER and REAL. The
`reach_cycle` shape plus a NOCASE domain reproduces the defect, and the
pg_ivm and differential dataflow columns say what the right answer is.

## Acceptance Criteria

- [ ] `makeCircuitFixture` takes a value domain
- [ ] every shape runs against all three domains
- [ ] `reach_cycle` under NOCASE goes red before the fixpoint fix and green
      after
