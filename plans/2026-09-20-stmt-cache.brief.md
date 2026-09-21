# Lane stmt-cache

One issue: `statement-cache-thrash`, **scoped to `src/1_maintenance.rs` only this
round**. Run `issuectl context statement-cache-thrash` for the bundle.

Base: `origin/main` @ `be8bae6`. Branch `perf/stmt-cache`.

## The measurement, already done. Do not re-profile

| prepare policy | fold, 2048 changes |
|---|---|
| re-preparing the maintenance statements per row | 163.2 ms |
| reusing them | 40.6 - 41.4 ms |

Re-preparing is about 75 percent of the fold. Every other queued lab prices at 6
percent or less. Source:
`labs/20260920.1.the-gang-finds-out-where-the-time-went/HYPOTHESIS.md`, verdict
2026-09-20, corner 8 tables x 20 columns, join arity 4, seed 42, median of three
legs.

## The scope cut

The issue names 7 fresh-prepare sites in `src/1_maintenance.rs` and 19 in
`src/1a_relational.rs`. **You take the 7 only.** A concurrent lane owns
`src/1a_relational.rs` and will conflict with you on every hunk. The other 19 get
a follow-up card once that lane lands. Say so in your PR body.

## The idiom already exists in this repo

`rusqlite` keeps an LRU of prepared statements keyed by SQL text. Seven existing
uses: `src/1a_relational.rs:26`, `:69`, `:133`, `:217`, `:871`, `src/2_vtab.rs:138`,
`:302`. Read one before you write yours.

`maintain()` builds each statement with `format!` and runs it through
`db.execute(&sql, ...)` or `db.query_row(&sql, ...)`, both of which prepare fresh
and drop. Per maintained row. The SQL text is stable per view per step, so the
cache hits. Rebuilding the string with `format!` each call costs an allocation and
does not miss the cache; leave that alone unless you measure it.

## Files you own

```
src/1_maintenance.rs
issues/statement-cache-thrash/item.md           (checkbox toggles only)
```

## Files you must not touch

```
src/1a_relational.rs    ← another lane is editing this file right now
src/0b_relational.rs    src/0_query.rs    src/0a_catalog.rs
src/2_vtab.rs           src/2a_source_ddl.rs    src/3_extension.rs
tests/**   bench/**   labs/**   docs/**   scripts/**   Cargo.toml   plans/**
issues/*/  (any other issue)
```

`tests/**` is read-only for you: the existing battery is your rail. If the change
needs a new test, stop and report that instead of writing one.

## Acceptance

- [ ] the 7 fresh-prepare sites in the `maintain` path use the statement cache
- [ ] the full battery green, three runs
- [ ] a before and after number on the same corner, median of three, pasted in
      the PR body
- [ ] the 19 remaining sites in `src/1a_relational.rs` named in the PR body as
      the follow-up

## Validation

```bash
cd /Users/chrishafley/projects/sqlite_ivm
cargo test 2>&1 | tail -30
```

Three runs. `tests/4_features.rs` takes 10 to 19 seconds and that variance is
pre-existing; report it, do not chase it. Nothing else in this lane may exceed 10s
in the foreground; background anything longer and poll.

Measure with the lab rig in
`labs/20260920.1.the-gang-finds-out-where-the-time-went/`, same corner and seed as
the verdict above, median of three. Measure as you go: one change, one number. No
batched rewrite followed by a single measurement at the end.

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
  with a comment saying what it protects, and a named diagnostic when it is hit.
- Never a per-row write. A formerly-quadratic path gets a COUNT or EXPLAIN test,
  additive only.
- Commit messages carry `Refs-Issue: @statement-cache-thrash`.

## Report back

`boop beep --no-wait --as stmt-cache sprefa-coordinator "<one line>"`

**Commit your work before you report done.** The last two lanes returned rc=0
with an uncommitted tree and I had to commit for them.

One line on landing: sites converted, before and after milliseconds.
