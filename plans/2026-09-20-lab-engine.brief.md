# Lane lab-engine

Two issues in `issuectl dag` lane `lab-engine`, strictly in order. The second is
blocked by the first because both rewrite the same two files.

| issue | size | order |
|---|---|---|
| `lab-drop-json-payload` | M | first, head-of-line |
| `lab-batch-at-xsync` | L | second, blocked by the first |

Run `issuectl context <slug>` for the full bundle on each.

## NO TIMING RUNS

Other agents are on this machine. A wall-clock number taken under load is worse than
no number, because it looks like evidence.

Do not run benchmarks. Do not run `bench/`. Do not report milliseconds. Every claim in
both issues is a **statement count**, asserted through `hafley-observe`'s
`CountRecorder`, which is deterministic and costs nothing.

If you believe a timing run is required, stop and say so.

## Files you own

```
src/1_maintenance.rs
src/2_vtab.rs
tests/1_maintenance.rs
tests/2_vtab.rs
issues/lab-drop-json-payload/item.md    (checkbox toggles only)
issues/lab-batch-at-xsync/item.md       (checkbox toggles only)
```

## Files you must not touch

```
labs/**        docs/**        scripts/**     plans/**
bench/**       Cargo.toml     src/0*.rs      src/1a_relational.rs
src/2a_source_ddl.rs          src/3_extension.rs
tests/0_*.rs tests/3_*.rs tests/4_*.rs tests/5_*.rs tests/6_*.rs
```

`tests/6_extension_load.rs` is being written right now by lane `lab-probe`.
`labs/**` belongs to lane `lab-rig`. `Cargo.toml` belongs to `lab-probe`; if you need
a dependency, stop and say so.

## Task 1: lab-drop-json-payload

The trigger serializes the changed row to JSON and the vtab parses it back. At M=20
that is 20 encodes in SQL, then `json_valid` + `json_type` + `json_array_length`, then
a `json_each` scan, then 20 `json_extract` calls. Four of the nine row-clock steps in
`docs/2026-09-19-vtab-clocks.md` exist only to undo the first one.

Declare one hidden column per used source column instead of one `__ivm_row TEXT`.

| site | what changes |
|---|---|
| `src/2_vtab.rs:167` | the `declare_vtab` string, built from the plan |
| `src/2_vtab.rs:386` | `insert()` reads typed argv slots instead of parsing JSON |
| `src/2_vtab.rs:389` | the read-only guard, whose offsets move |
| `src/1_maintenance.rs:399` | the trigger body binds values instead of `json_array` |
| `src/1_maintenance.rs:118` | `json_extract` per column goes away |

The one real design decision: the hidden-column count varies per source table, so the
declared schema is built from the plan rather than fixed.

The guard at `src/2_vtab.rs:389` is the access control that separates "someone is
maintaining" from "someone is writing the view." Widening the schema is exactly where
that breaks. Its test is in the plan and is not optional.

## Task 2: lab-batch-at-xsync

Every `xUpdate` runs the whole maintenance query inline today: delta wipe, contributions
insert, `GROUP BY`. Per row. An UPDATE pays it twice (`src/1_maintenance.rs:388`).

Buffer in `xUpdate`, flush in `xSync`. This is not a new design. FTS5 does it in tree,
and `docs/2026-09-19-fts5-clock-choices.md` has every site:

| question | answer | site |
|---|---|---|
| where does the flush go | `xSync` | `fts5.c:21203` |
| what about `xCommit` | no-op; SQLite discards its return code | `fts5.c:21215` |
| how is the buffer bounded | byte cap, 1 MiB default, plus an ordering trigger | `fts5.c:4548`, `:16326` |
| savepoint, release | flush | `fts5.c:22248`, `:22265` |
| rollback-to | discard | `fts5.c:22283` |

Read that document before writing the buffer. Line numbers are into the local
`libsqlite3-sys-0.38.2` amalgamation, SQLite 3.53.2.

**The bound is mandatory.** An unbounded buffer is a blocking defect under the
every-loop-is-bounded law. Named constant, comment saying what it protects, early
flush tested.

**The trap the plan names:** a buffer that passes every correctness test by flushing on
every `xUpdate` anyway. The `CountRecorder` assertion is what catches it. Write that
assertion first.

## Validate

```bash
cargo test --locked --test 1_maintenance
cargo test --locked --test 2_vtab
cargo test --locked
bash scripts/9_verify.sh
```

`scripts/9_verify.sh` builds the extension and runs the CLI scenarios; it is the outer
gate and it is correctness, not performance.

The existing battery must be green and unchanged. A test edited to accommodate the new
shape is a finding to report, not a thing to do quietly.

`.github/CI-KNOWN-RED.md` lists legs allowed to be red. Read it before reporting
anything broken. Measure a leg three times, never once, and never under lane load.

## Style laws

- Comment budget: only constraints the code cannot show. No change-log narrative,
  no dates, no arc references.
- `eprintln!` never in `src/**`. `tracing` only.
- Every loop and every recursion carries an explicit budget and a named diagnostic
  when the budget is hit. The bound is a constant with a comment saying what it protects.
- Colocated consistency: inside a file, follow that file's style.
- No em dashes. Banned in prose and identifiers: provenance, substrate, load-bearing,
  regime, "ground truth" (say oracle), refusal. "support" is banned, use refCount.
- Surrogate INTEGER keys; natural TEXT keys once in a dictionary table.
- Never a per-row write.
- A compiler error for an unbuilt construct is "not built yet" with the throw site
  cited. Never report it as a language limit.

## Kernel rule

These two change the trigger wire format and the vtab schema. `POLICY.md` says upgrades
rebuild rather than migrate, so there is no migration to write. If you find yourself
writing one, stop: that means the rebuild path is missing and that is a different issue.

## Receipts

Close checkboxes with `issuectl check <slug> "<substring>"`. Never hand-edit frontmatter.
Record commits with a `Refs-Issue: @<slug>` trailer.

Report: `git log --oneline`, `git status`, `git diff --stat origin/main...HEAD` showing
only owned files, the `CountRecorder` before-and-after numbers, and the three separate
runs of the slow leg.

`rc=0` proves nothing. Grade your own tree before you say it is done.
