# Independent Rust engine lane

Goal: implement one reusable in-process Rust frontier engine in an isolated lab, then report its actual contract and limits.

Read all of `0_BRIEF.md`, `1_CASES.md`, `4_RUST_PROMPT.md`, and both oracle SQL and expected TSV pairs before editing. Review the packet for missing constraints or contradictions and hail the coordinator with concrete questions or proposed additions. Continue independent work that does not depend on an answer.

Own only `labs/20260923.1.the-gang-builds-the-rust-frontier-engine/` in your worktree. Create its own Cargo workspace. Do not edit the input packet, the direct-pass lab `20260923.0`, Sprefa, `sqlite_ivm` production code, or `hafley-rs`. Do not copy implementation code from the direct-pass lab; derive from the packet and source libraries. Use the correlated `hafley-observe` crate for observations. Keep rows typed, row identity integer, join lookup indexed, and signed changes consolidated at one frontier. No JSON or serialized row keys. The grant graph cannot be the only accepted plan shape; grouped COUNT/SUM is the second gate.

Write the public type signatures before internals in your report. Follow with installed-program and frontier lifetimes, then storage and exact read/write sequence. For each associated type, state two real call sites or a compile-time constraint. Use the same public API for both cases. Explicitly return unsupported for missing required shapes.

Validation: run standalone oracle commands in `0_BRIEF.md`; run `cargo fmt --check`, `cargo test --offline`, and the runnable example in this lab. Compare every committed snapshot and signed delta against both TSV pairs. Test support 2→1 and 1→0, the simultaneous join cross-term, rollback, update, empty group, duplicate rows, and net-zero batch. Measure repeated frontiers, process RSS, installed operator/arrangement counts. Record exact command, output, failures, and source revisions in `HYPOTHESIS.md`.

Deliver one or more scoped commits with `Boop-Status: done` only after checks pass. Send a compact receipt: status, SHA, files, validation, and next. Stop and hail the coordinator on a scope change or a blocker; leave unrelated worktrees and uncommitted files alone.
