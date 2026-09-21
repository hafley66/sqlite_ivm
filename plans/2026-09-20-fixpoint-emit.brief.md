# Lane fixpoint-emit

One issue: `fixpoint-emits-rederived-rows`. Run `issuectl context
fixpoint-emits-rederived-rows` for the bundle.

Base: `origin/main` @ `be8bae6`. Branch `fix/fixpoint-emit`.

## The defect

Reproduced. `probes/2026-09-19-fixpoint-retraction/repro.sql` on main.

The fixpoint delete path removes rows from the member table `all` by `__k` and
discards the stored raw values, re-derives replacements into `work` by evaluating
the rule head afresh, then builds the retraction from `work`. When two raw rows
share a `__k` but differ byte-wise, the re-derived representative is not the one
that was stored. Downstream looks the row up by `__r`, which is byte and bit
exact, and finds nothing.

`__k` treats rows as equal under NOCASE and when `key()` at `src/1a_relational.rs:35`
folds an integral real to an integer, so both are reachable without exotic input.

## Root cause, already established. Do not re-derive it

The member table is the one arrangement with a different shape: `__k` only, no
`__r`, no `__n`. The delete path had no stored identity to emit and no written
rule said which value it must send.

Two equality encodings is correct design and is **not** the cause. A lane
already proved Rust `key()` and SQL `key_sql()` agree across every scalar class
and all three collations (`tests/7_key_agreement.rs`, on main). Do not go
re-audit the encoders.

## The invariant

Every operator emits the row stored in its arrangement. Exactly one operator
breaks it. Write that invariant at the top of `src/1a_relational.rs` in two
sentences, then make the fixpoint obey it.

## Failure modes to check you did not trade one for another

- `missing result multiplicity` at `src/1a_relational.rs:433`
- `negative arrangement multiplicity` at `:113`, when the fixpoint feeds
  another arrangement
- silent divergence when the error is swallowed after a partial emit: `emit` at
  `:413-445` has no savepoint, and `all` plus the input arrangement are already
  mutated by then

## Files you own

```
src/1a_relational.rs                            (the Kind::Fixpoint branch and its emit path)
tests/9_fixpoint_retraction.rs                  (new)
issues/fixpoint-emits-rederived-rows/item.md    (checkbox toggles only)
```

## Files you must not touch

```
src/0b_relational.rs   src/0_query.rs   src/0a_catalog.rs
src/1_maintenance.rs   src/2_vtab.rs    src/2a_source_ddl.rs   src/3_extension.rs
bench/**   labs/**   docs/**   scripts/**   Cargo.toml   plans/**
tests/0_*.rs tests/1_*.rs tests/2_*.rs tests/3_*.rs tests/4_*.rs
tests/5_*.rs tests/6_*.rs tests/7_*.rs tests/8_*.rs
issues/*/  (any other issue)
```

A concurrent lane owns `src/1_maintenance.rs` and `src/2_vtab.rs`. Do not touch
them even if the fix looks like it wants to.

Inside `src/1a_relational.rs`, another change recently landed in the `Kind::Group`
branch (the `min(__n, limit+offset)` clamp at two sites). Leave it alone.

## Acceptance

- [ ] `probes/2026-09-19-fixpoint-retraction/repro.sql` runs clean
- [ ] `nocase.py` and `reals.py` in that probe directory report OK for every shape
- [ ] the invariant written at the top of `src/1a_relational.rs`, two sentences
- [ ] the oracle restated: recursive and DISTINCT outputs match up to `__k`
      equality with representatives normalized before comparison, so fuzz stops
      reporting representative flips as defects
- [ ] `tests/9_fixpoint_retraction.rs` is red on `be8bae6` and green after.
      Paste both runs in the PR body.

## Validation

```bash
cd /Users/chrishafley/projects/sqlite_ivm
sqlite3 :memory: < probes/2026-09-19-fixpoint-retraction/repro.sql
python3 probes/2026-09-19-fixpoint-retraction/nocase.py
python3 probes/2026-09-19-fixpoint-retraction/reals.py
cargo test --test 9_fixpoint_retraction 2>&1 | tail -20
cargo test 2>&1 | tail -30
```

Run the full battery three times. `tests/4_features.rs` takes 10 to 19 seconds and
that variance is pre-existing; report it, do not chase it. Nothing else in this
lane may exceed 10s in the foreground; background anything longer and poll.

## Style laws, inline

- No em dashes anywhere, prose or identifiers.
- Banned words: provenance, substrate, load-bearing, regime, "ground truth" (say
  oracle), "support" as a noun (say refCount). Applies to identifiers too.
- No negative parallelism ("not X, Y"). No rhetorical closes. No one-word sentences.
- Comment budget: at most two consecutive comment lines, and only constraints the
  code cannot show. A hook enforces this and will block your edit. No change-log
  narrative, no dates, no arc references.
- Colocated consistency: inside a file, follow that file's existing style.
- `eprintln!` never in `src/**`; `tracing` only.
- Every loop and every recursion carries an explicit budget as a named constant
  with a comment saying what it protects, and stops with a named diagnostic when
  the budget is hit. The fixpoint already has `fixpoint closure round budget
  exceeded`; keep it.
- A compiler error for an unbuilt construct is "not built yet" with the throw site
  cited. Never call it a language limit.
- Commit messages carry `Refs-Issue: @fixpoint-emits-rederived-rows`.

## Report back

`boop beep --no-wait --as fixpoint-emit sprefa-coordinator "<one line>"`

**Commit your work before you report done.** The last two lanes returned rc=0
with an uncommitted tree and I had to commit for them.

One line on landing: which value the delete path now emits, and where the
invariant sentence sits.
