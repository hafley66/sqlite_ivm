# Lane key-agreement

One issue: `key-encoding-agreement-test`. Run `issuectl context
key-encoding-agreement-test` for the bundle.

Base: `origin/main` @ `43d695e`. Branch `feat/key-agreement`.

## Why this exists

Three encoders exist for one concept.

| encoder | site | reals as |
|---|---|---|
| Rust `key()` | `src/1a_relational.rs:35` | folded to integer when integral |
| Rust `identity()` | `src/1a_relational.rs:52` | `json_object('real', <hex bits>)` |
| SQL `key_sql()` | `src/0b_relational.rs:177` | `json_object('real', printf('%!.17g'))` |

The Rust pair feeds Set, Join, Group and the fixpoint input side. `key_sql`
feeds only `all` and `work`. They never cross-compare today, so there is no bug
today. Nothing states that. One future join between those tables is a silent
wrong answer with no error.

A later lane replaces these TEXT keys with an interned INTEGER (decision
recorded at `issues/json-text-keys-in-indexes/item.md`). Your test is what
catches that rewrite drifting. Write it so it still means something after the
encoders change shape.

## Files you own

```
tests/7_key_agreement.rs                        (new)
tests/support/**                                (additions only, no rewrites)
issues/key-encoding-agreement-test/item.md      (checkbox toggles only)
```

## Files you must not touch

```
src/**          bench/**        labs/**         docs/**
scripts/**      Cargo.toml      plans/**        issues/*/  (any other issue)
tests/0_*.rs tests/1_*.rs tests/2_*.rs tests/3_*.rs
tests/4_*.rs tests/5_*.rs tests/6_*.rs
```

`src/**` is read-only for you. If the test needs a `pub(crate)` opened up,
stop and report it instead of opening it.

## The corpus

A manual audit on 2026-09-19 compared the bulk SQL path and the incremental
Rust path and found agreement on every class below. That audit is not a test
and will not stay true. Turn it into one.

```
integers, integral reals, non-integral reals, negative zero, NaN,
positive and negative infinity, the i64::MAX boundary, integers beyond
f64 exact range, empty blobs, null-byte blobs, text that mimics the
tagged blob encoding, NULL
```

Cross those against affinity and collation: BINARY, NOCASE, RTRIM.

## The open case

One PLAUSIBLE finding from that audit, unverified: a Group key expression
returning JSON-subtyped text is embedded as JSON by the bulk path and quoted as
a string by the incremental path, so `json_extract(j,'$.t')` over `'{"t":["a"]}'`
yields `[["a"]]` against `["[\"a\"]"]`. It depends on whether the planner admits
expression group keys. Read `src/0b_relational.rs` and answer it. Confirmed or
rejected, with the throw site or the passing case cited. Do not leave it open.

## Acceptance

- [ ] a property test asserts Rust `key()` after `key_expression` equals SQL
      `key_sql()` across the corpus and all three collations
- [ ] `identity()` round-trips across the corpus
- [ ] the JSON-subtype group key case confirmed or rejected, with a citation
- [ ] which encoder keys which table written down, in the test file header,
      not in a doc

## Validation

```bash
cd /Users/chrishafley/projects/sqlite_ivm
cargo test --test 7_key_agreement 2>&1 | tail -20
cargo test 2>&1 | tail -30
```

Run the new leg three times. Nothing in this lane may take over 10s in the
foreground; background anything longer and poll.

## Style laws, inline

- No em dashes anywhere, prose or identifiers.
- Banned words: provenance, substrate, load-bearing, regime, "ground truth" (say
  oracle), "support" as a noun (say refCount). Applies to identifiers too.
- No negative parallelism ("not X, Y"). No rhetorical closes. No one-word sentences.
- Comment budget: only constraints the code cannot show. No change-log narrative,
  no dates, no arc references in comments.
- Colocated consistency: inside a file, follow that file's existing style.
- `eprintln!` never in `src/**`; `tracing` only.
- Every loop and every recursion carries an explicit budget and a named diagnostic
  when the budget is hit. A generated corpus is a loop: bound it with a named
  constant and say what the constant protects.
- Commit messages carry `Refs-Issue: @key-encoding-agreement-test`.

## Report back

`boop beep --no-wait --as key-agreement sprefa-coordinator "<one line>"`

One line on landing: the corpus size, the collations covered, and the verdict on
the JSON-subtype case.
