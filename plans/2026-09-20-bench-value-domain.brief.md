# Lane bench-value-domain

One issue: `bench-value-domain-axis`. Run `issuectl context bench-value-domain-axis`
for the bundle.

Base: `origin/main` @ `43d695e`. Branch `test/bench-value-domain`.

## Why this exists

The rig covers 20 shapes x 4 implementations including deletes and a
`retract_one_support` state, and it still missed `fixpoint-emits-rederived-rows`.
`bench/shared/30_circuit_workload.mjs:3` says why: "Fixture values are bounded
integers". Distinct small integers make `__k` and `__r` agree on every row, so
the two notions of sameness never diverge and the defect is structurally
invisible.

Your job is the missing axis, not the engine fix. Another lane owns the fix.
When your axis is in, `reach_cycle` under NOCASE goes red on `origin/main`.
Red is the deliverable.

## Files you own

```
bench/shared/30_circuit_workload.mjs
bench/shared/36_semantic_catalog.mjs
bench/shared/*                                  (only if the domain must thread through)
issues/bench-value-domain-axis/item.md          (checkbox toggles only)
```

## Files you must not touch

```
src/**          tests/**        labs/**         docs/**
scripts/**      Cargo.toml      plans/**        issues/*/  (any other issue)
bench/44_pg_ivm_1_15_expected.json              (frozen golden)
bench/44a_pglite_1_13_expected.json             (frozen golden)
```

## The change

`makeCircuitFixture` takes a value domain. Every shape runs against all three:

| domain | what it makes `__k` vs `__r` do |
|---|---|
| `integers` | today's behaviour, the baseline that must stay byte-identical |
| `text_nocase` | two raw rows share a `__k` under NOCASE, differ byte-wise |
| `mixed_int_real` | `key()` folds an integral real to an integer, `identity()` does not |

The sha256-per-expected-state stays the oracle. Each domain gets its own
expected set; the `integers` set must come out byte-identical to what is on
`origin/main` today. If it does not, you changed the generator, not the axis.

## Acceptance

- [ ] `makeCircuitFixture` takes a value domain
- [ ] every shape runs against all three domains
- [ ] `reach_cycle` under NOCASE red on `43d695e`, and you paste the diff of
      expected vs actual rows into the PR body
- [ ] `integers` expected states byte-identical to `43d695e`

## Validation

```bash
cd /Users/chrishafley/projects/sqlite_ivm
node bench/42_feature_run.mjs 2>&1 | tail -40
node bench/53_shootout.mjs 2>&1 | tail -40
```

Run each three times. A leg that flaps is a finding, report it, do not average it.
Nothing in this lane may take over 10s in the foreground; background anything longer
and poll.

## Style laws, inline

- No em dashes anywhere, prose or identifiers.
- Banned words: provenance, substrate, load-bearing, regime, "ground truth" (say
  oracle), "support" as a noun (say refCount). Applies to identifiers too.
- No negative parallelism ("not X, Y"). No rhetorical closes. No one-word sentences.
- Comment budget: only constraints the code cannot show. No change-log narrative,
  no dates, no arc references in comments.
- Colocated consistency: inside a file, follow that file's existing style.
- Every loop and every recursion carries an explicit budget and a named diagnostic
  when the budget is hit. A fixpoint with no budget is a blocking defect.
- Commit messages carry `Refs-Issue: @bench-value-domain-axis`.

## Report back

`boop beep --no-wait --as bench-value-domain sprefa-coordinator "<one line>"`

One line on landing: the shape that went red, the domain, the row that differs.
