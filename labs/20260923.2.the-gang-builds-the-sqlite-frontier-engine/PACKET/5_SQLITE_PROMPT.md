# SQLite extension worktree prompt

Read `0_BRIEF.md`, `1_CASES.md`, `2_oracle.sql`, `3_expected.tsv`, and `3a_deltas.tsv` completely. Implement a reusable SQLite-backed incremental relational engine as a library plus a loadable extension in an independent Cargo workspace. Use the correlated `sqlite-ext` crate for plugin entry, callback support, statement spans, and transactional row collection. Record its revision. Do not copy its source into this lab.

Begin by writing the public trait signatures and concrete types. Choose associated types and generics based on real caller requirements. State which operations are Rust calls and which cross the SQLite extension boundary. State the lifetime of the connection, installed plan, collector, and one transaction frontier. Show tables, indexes, catalog rows, and the read/write order before coding maintenance.

SQLite stores one value per column. Keep source row identity distinct from SQLite value equality. Use integer IDs for internal arrangements and joins, typed cells for values, and signed support counts for set visibility. No JSON payloads, stringified row keys, or serialized-row join keys. Source writes in one transaction must reach one settled frontier; preserve savepoint and transaction rollback semantics. An error during maintenance must leave the previous committed state readable.

Run every gate in `0_BRIEF.md` through the public API and through a loaded native extension, including `3b_aggregate.sql` and its expected files. Record generated SQL, its byte length, statement PROFILE counters, object and index counts, allocator memory, database/WAL size, and Rust RSS. Deliver a general SQL-facing extension; Sprefa rule IDs, rule heads, and term-arena types cannot enter the plugin API.
