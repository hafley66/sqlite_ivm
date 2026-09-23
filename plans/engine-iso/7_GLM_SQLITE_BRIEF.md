# Independent SQLite extension engine lane

Goal: implement one reusable SQLite-backed frontier engine as a library and a loadable extension in an isolated lab, then report its actual contract and limits.

Read all of `0_BRIEF.md`, `1_CASES.md`, `5_SQLITE_PROMPT.md`, and both oracle SQL and expected TSV pairs before editing. Review the packet for missing constraints or contradictions and hail the coordinator with concrete questions or proposed additions. Continue independent work that does not depend on an answer.

Own only `labs/20260923.2.the-gang-builds-the-sqlite-frontier-engine/` in your worktree. Create its own Cargo workspace. Do not edit the input packet, the direct-pass lab `20260923.0`, Sprefa, `sqlite_ivm` production code, or `hafley-rs`. Do not copy implementation code from the direct-pass lab; derive from the packet and source libraries. Depend on the correlated `sqlite-ext` and `hafley-observe` crates, never vendor their source. Keep SQLite cells typed, row identity integer, join lookup indexed, and signed changes consolidated at one frontier. No JSON or serialized row keys. The grant graph cannot be the only accepted plan shape; grouped COUNT/SUM is the second gate.

Write public type signatures before internals in your report. Follow with connection, installed-plan, collector, and frontier lifetimes, then tables, indexes, catalog state, and exact read/write sequence. Explain which calls cross the loadable-extension boundary. Keep rule names, heads, and Sprefa terms out of the plugin API. Explicitly return unsupported for missing required shapes.

Validation: run standalone oracle commands in `0_BRIEF.md`; run `cargo fmt --check`, `cargo test --offline`, a runnable example, and integration tests that load the native extension into a host SQLite connection. Compare every committed snapshot and signed delta against both TSV pairs. Test support 2→1 and 1→0, the simultaneous join cross-term, rollback, savepoint rollback, callback failure, update, empty group, duplicate rows, and net-zero batch. Record generated SQL, SQL bytes, PROFILE counters, prepare count over repeated frontiers, object and index counts, allocator memory, database/WAL size, Rust RSS, exact commands, failures, and source revisions in `HYPOTHESIS.md`.

Deliver one or more scoped commits with `Boop-Status: done` only after checks pass. Send a compact receipt: status, SHA, files, validation, and next. Stop and hail the coordinator on a scope change or a blocker; leave unrelated worktrees and uncommitted files alone.
