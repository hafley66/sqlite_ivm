# Two engine ISO experiment

This directory is the input packet for two independent implementation worktrees. Copy it into each worktree unchanged. Both implementations must produce reusable library code and run the same cases; a demonstration binary alone does not satisfy the assignment.

## Inputs

| File | Role |
| --- | --- |
| `1_CASES.md` | Typed relations, frontier sequence, required observations |
| `2_oracle.sql` | Standalone SQLite recomputation oracle |
| `3_expected.tsv` | Oracle output at every committed frontier |
| `3a_deltas.tsv` | Net signed output per frontier, including empty frontiers |
| `3b_aggregate.sql` | Independent grouped COUNT/SUM oracle |
| `3c_aggregate_expected.tsv` | Grouped result at every committed frontier |
| `3d_aggregate_deltas.tsv` | Signed grouped-result changes |
| `4_RUST_PROMPT.md` | In-process Rust implementation assignment |
| `5_SQLITE_PROMPT.md` | SQLite extension implementation assignment |

The files use relative paths and no checkout-specific build command. Each worktree records its source revision, dependency versions, commands, and results in its own `HYPOTHESIS.md` or report. The implementation is an ISO lab: its own Cargo workspace, no path dependency on `sqlite_ivm` or Sprefa, and no edits to either entry point. A SQLite implementation may depend on the correlated `sqlite-ext` checkout; record the exact revision and path in the report.

## Common output

Each worktree delivers a library crate with a public trait or generic API, a concrete implementation, tests through that public API, and a runnable example. Place type signatures first in the report, then instance lifetimes, then storage layout and read/write sequence. Explain every associated type and generic parameter with at least two call sites or one concrete type-level constraint. Remove parameters that only rename a fixed type.

The API must accept a complete frontier of signed source changes and return only after the frontier is settled. It must expose a stable snapshot and the net signed output changes. Its error result must identify the stage and relation involved. Program installation and teardown are separate from source-change application. The implementation must keep relation identities and row cells typed; output row identity and join lookup use indexed integer IDs. No JSON payloads, stringified row keys, or serialized-row join keys.

The primary case is `1_CASES.md`; `3b_aggregate.sql` supplies a second, structurally different case. Run both through the same public API without changing its method signatures. Include an explicit unsupported result for any required shape that cannot yet run; no success-shaped fallback to full recomputation.

## Gates

1. Compare every committed snapshot and signed output frontier against both pairs of expected TSVs. Check the no-output frontiers explicitly.
2. Assert the simultaneous join-input batch produces its cross-term once. Assert support transitions 2 to 1 emit no output change, and 1 to 0 emit one retraction.
3. Check update as old-row retraction plus new-row addition, transaction rollback, savepoint rollback, and a callback error that leaves the previous committed snapshot readable.
4. Use the same public API for the grouped query. Include an empty source, duplicate source rows where allowed, and a batch that nets to zero.
5. Expose observation through `hafley-observe`. Record frontier ID, input and output counts, operator or maintenance span counts, SQL bytes and SQLite PROFILE counters where applicable, Rust process RSS, and SQLite allocator/database/WAL space where applicable. Put application-specific event names in the implementation crate.
6. Record installed object counts, indexed arrangements, prepare count, and repeated-frontier work. State which counts are contractual guardrails and which timings are measurements. Run paired repeated measurements only after correctness gates pass.
7. Run library tests, the example, and the standalone oracle command from a clean worktree. Record failures as failures. Do not copy the other implementation into this worktree.

## Oracle command

```bash
cd plans/engine-iso
sqlite3 -batch -noheader -separator $'\t' :memory: < 2_oracle.sql > /tmp/engine-iso-actual.tsv
diff -u 3_expected.tsv /tmp/engine-iso-actual.tsv
sqlite3 -batch -noheader -separator $'\t' :memory: < 3b_aggregate.sql > /tmp/engine-iso-aggregate-actual.tsv
diff -u 3c_aggregate_expected.tsv /tmp/engine-iso-aggregate-actual.tsv
```

The implementation may use differential-dataflow, SQLite, and existing shared crates. Testing helpers belong in `hafley-observe`; compiler-specific rules and terms stay outside `sqlite-ext` and `sqlite_ivm`.
