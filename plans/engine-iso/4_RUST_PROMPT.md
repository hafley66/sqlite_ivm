# In-process Rust worktree prompt

Read `0_BRIEF.md`, `1_CASES.md`, `2_oracle.sql`, `3_expected.tsv`, and `3a_deltas.tsv` completely. Implement a reusable in-process incremental relational engine as a library in an independent Cargo workspace. The grant case is an input, not the architecture. Keep it separate from Sprefa and `sqlite_ivm`.

Begin by writing the public trait signatures and concrete types. Use associated types for the row/value domain, plan identity, and error only where they allow a real alternate implementation or preserve a compile-time invariant. State the lifetime of an installed program and one frontier. Show the storage layout, arrangement indexes, and the exact writes for each frontier before coding the internals.

Use signed differences and consolidate them at the frontier. Implement the join batch equation, including its cross-term, and set visibility over support counts. Preserve source row identity separately from value equality. Avoid serialized rows as keys, string interning on the hot path, and full-query recomputation after each frontier. Use indexed integer identities for arrangements and expose operator counts in `hafley-observe` spans.

Run every gate in `0_BRIEF.md` through the public API, including `3b_aggregate.sql` and its expected files. Record the complete test and measurement commands, results, and any unsupported operation. Deliver code intended to be used by a caller crate, not a one-case `main.rs`.
