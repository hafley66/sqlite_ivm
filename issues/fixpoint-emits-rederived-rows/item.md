---
created: 2026-09-19
updated: 2026-09-19
type: bug
status: open
priority: high
epic: ivm-correctness-and-storage
labels: [correctness, reproduced]
---

# Fixpoint deletion emits re-derived rows, not stored ones

## Description

Reproduced. `probes/2026-09-19-fixpoint-retraction/repro.sql` on branch
`test/retraction-repro`, commit `c0ee98e`.

## What happens

`src/1a_relational.rs:846` deletes rows from the member table `all` by `__k`
and discards the stored raw values. `:849-864` re-derives replacements into
`work` by evaluating the rule head afresh. `:902-906` builds the retraction
from `work`.

When two raw rows share a `__k` but differ byte-wise, the re-derived
representative is not the one that was stored. Downstream looks the row up by
`__r`, which is byte and bit exact, and finds nothing.

`__k` treats rows as equal under a NOCASE collation and when `key()` at `:35`
folds an integral real to an integer, so both are reachable without exotic
input.

## Failure modes

- `missing result multiplicity` at `src/1a_relational.rs:433`
- `negative arrangement multiplicity` at `:113` when the fixpoint feeds
  another arrangement
- silent divergence when the error is swallowed after a partial emit, because
  `emit` at `:413-445` has no savepoint and `all` plus the input arrangement
  were already mutated

## Root cause

The member table at `:299` is the one arrangement with a different shape:
`__k` only, no `__r`, no `__n`. The delete path had no stored identity to
emit, and no written invariant said which value it must send. Having two
equality encodings is correct design and is not the cause.

## Acceptance Criteria

- [ ] `probes/2026-09-19-fixpoint-retraction/repro.sql` runs clean
- [ ] `nocase.py` and `reals.py` report OK for every shape
- [ ] the invariant is written at the top of `src/1a_relational.rs`
- [ ] the oracle is restated: recursive and DISTINCT outputs match up to `__k`
      equality, with representatives normalized before comparison
