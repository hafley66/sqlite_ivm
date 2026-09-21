# Lane intern-keys

One issue: `json-text-keys-in-indexes`. Run `issuectl context
json-text-keys-in-indexes` for the bundle.

Base: `origin/main` @ `8150541`. Branch `feat/intern-keys`.

This is the largest card in the epic. Read the whole brief before editing.

## The decision, already made by the user. Do not re-litigate it

A dictionary table maps a surrogate INTEGER key to the composite. Arrangement
tables and every index key on that integer. Queries that need the composite back
join the dictionary; queries that need to search inside it get an index on the
dictionary, not a scan.

| option | verdict |
|---|---|
| intern in a dictionary table, key by rowid | **chosen** |
| N separate key columns plus a composite index | rejected, arity is per-node, the DDL stops being uniform |
| hash to INTEGER | rejected, collisions need a tiebreak column |
| keep TEXT | the measured baseline |

## The shape today

`src/1a_relational.rs:266` and `:284`:

```sql
CREATE TABLE "v_op417_1"(
  __k TEXT NOT NULL,          -- json_array string, group and join key
  __r TEXT NOT NULL UNIQUE,   -- json_array string, row identity
  __n INTEGER NOT NULL,
  c0, c1, c2
);
CREATE INDEX ON "v_op417_1"(__k);
```

Columns are real columns; rows are not stuffed into cells. The keys are the
problem. Every b-tree entry keys on a variable-length TEXT value instead of an
8-byte integer, on two indexes, on every operator table, on every write. The same
composite is serialized twice per row and both copies are indexed. `:266` repeats
the shape with `__key TEXT`. The fixpoint member table at `:314` is `__k TEXT NOT
NULL UNIQUE` with no `__r` and no `__n`, a third shape.

Three encoders feed these: Rust `key()` at `:35`, Rust `identity()` at `:52`, SQL
`key_sql()` at `src/0b_relational.rs:177`.

## Measure as you go. This is a hard requirement

One change, one number. No batched rewrite followed by a single measurement at
the end. The user named this explicitly.

Read `.claude/skills/sqlite-costs` before your first edit. It holds the measured
b-tree write rates on this machine by key shape and lists optimizations already
disproven here. Read `.claude/skills/sql-relational-design` for the key law.

Suggested order, each landing with its own number:

1. The dictionary table and its two operations (intern a composite, resolve an
   id back). No arrangement changes yet. Measure the intern cost alone.
2. `__k` becomes INTEGER on one operator kind. Measure that kind.
3. The rest of the operator kinds.
4. `__r` becomes INTEGER.
5. `_state` and the fixpoint member table.

If a step's number goes the wrong way, stop and report it. Do not push through
to the end and average.

## The invariant you must not break

`src/1a_relational.rs:1-2`, landed in #16:

> Every operator emits the row stored in its arrangement.
> Equality keys select membership, while stored row values select emitted identity.

Interning changes what a key **is**, not what gets emitted. If your change makes
an operator emit a resolved-from-dictionary row rather than its stored row, you
have reintroduced the bug that card closed.

## Rails that already exist. Use them, do not rewrite them

| test | what it pins |
|---|---|
| `tests/7_key_agreement.rs` | Rust `key()` equals SQL `key_sql()` across every affinity and all three collations, and `identity()` round-trips |
| `tests/9_fixpoint_retraction.rs` | the retraction oracle, up to `__k` equality with representatives normalized |
| `tests/8_group_limit.rs` | 160 Group LIMIT cases against plain SQL |
| `tests/10_growth.rs` | one `maintain` span per changed row, counts not durations |

`tests/7_key_agreement.rs` is the one that will catch you. It is expected to need
updating, since the encoders change shape. Update what it asserts, never delete a
case, and say in the PR body which assertions changed and why.

Two tests in tree are `#[ignore]`d against open defects:
`tests/8_group_limit.rs::window_with_limit_reads_every_copy` and the JSON-subtype
case in `tests/7_key_agreement.rs`. Leave both ignored. They are other cards.

## Files you own

```
src/1a_relational.rs
src/0b_relational.rs
tests/7_key_agreement.rs                        (assertion updates, no deletions)
tests/11_intern.rs                              (new)
issues/json-text-keys-in-indexes/item.md        (checkbox toggles only)
```

## Files you must not touch

```
src/1_maintenance.rs   src/2_vtab.rs   src/2a_source_ddl.rs   src/3_extension.rs
src/0_query.rs         src/0a_catalog.rs
bench/**   labs/**   docs/**   scripts/**   Cargo.toml   plans/**
tests/0_*.rs tests/1_*.rs tests/2_*.rs tests/3_*.rs tests/4_*.rs
tests/5_*.rs tests/6_*.rs tests/8_*.rs tests/9_*.rs tests/10_*.rs
issues/*/  (any other issue)
```

## Storage law, inline

- Surrogate INTEGER keys. Natural TEXT keys live once, in a dictionary table.
- No composite TEXT primary keys. No stringly-typed values.
- Never a per-row write. A formerly-quadratic path gets a COUNT or EXPLAIN test,
  additive only.
- A view is a cache: upgrades rebuild rather than migrate (`POLICY.md`). You do
  not need a migration path for the old TEXT layout. Rebuilding is the answer.

## Acceptance

- [ ] a dictionary table with a surrogate INTEGER key, indexed for the lookups
      the engine actually issues
- [ ] `__k` and `__r` on every arrangement table are INTEGER
- [ ] b-tree write rates measured per step against the TEXT baseline
- [ ] the frozen goldens stay byte-identical, or the PR body says which changed
      and why that is correct
- [ ] `tests/11_intern.rs` covers intern, resolve, and a collision-free round
      trip over the `tests/7_key_agreement.rs` corpus

## Validation

```bash
cd /Users/chrishafley/projects/sqlite_ivm
cargo test 2>&1 | tail -30
```

Three runs. `tests/4_features.rs` takes 9 to 19 seconds and that variance is
pre-existing; report it, do not chase it. Nothing else may exceed 10s in the
foreground; background anything longer and poll.

## Style laws, inline

- No em dashes anywhere, prose or identifiers.
- Banned words: provenance, substrate, load-bearing, regime, "ground truth" (say
  oracle), "support" as a noun (say refCount). Applies to identifiers too.
- dl8 vocabulary: products and rows, never "rel". Namespaces `gh.` `git.` `http.`
  `fs.` `oai.` `cli.`.
- No negative parallelism ("not X, Y"). No rhetorical closes. No one-word sentences.
- Comment budget: at most two consecutive comment lines, and only constraints the
  code cannot show. A hook enforces this and will block your edit.
- Colocated consistency: inside a file, follow that file's existing style.
- `eprintln!` never in `src/**`; `tracing` only.
- Every loop and every recursion carries an explicit budget as a named constant
  with a comment saying what it protects, and a named diagnostic when it is hit.
- A compiler error for an unbuilt construct is "not built yet" with the throw site
  cited. Never call it a language limit.
- Commit messages carry `Refs-Issue: @json-text-keys-in-indexes`.

## Report back

`boop beep --no-wait --as intern-keys sprefa-coordinator "<one line>"`

**Commit your work before you report done**, and commit each measured step
separately so the numbers are readable in `git log`.

One line on landing: the dictionary shape, and the write-rate number before and
after.
