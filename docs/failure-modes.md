# Failure modes

One row per incident that bit: what happened, the root cause, the test that was red before the fix, the rail that keeps it fixed.

| incident | root cause | fail-pre-fix test | rail |
|---|---|---|---|
| `8_group_limit` 3.7s, 73% of group_limit statements freshly prepared | `db.prepare`/`execute`/`execute_batch(format!)` in the drain path, one prepare per view per drain (`src/1a_relational.rs` drain, materialize) | `scripts/statement-costs.sh 8_group_limit`, `run=1` on 265545 events | every drain-path statement goes through `prepare_cached`; `plans/costs/README.md` ledger |
| a recursive view emitted 20 statements per source row | `Kind::Fixpoint` drained one delta row at a time through `change()` and a row-bound derive | `tests/13_statements_per_drain.rs` `reach_view: 189 at 8 rows, 2589 at 128` | the same test demands equal statement counts at 8 and 128 rows past the seed node |
| set-at-a-time fixpoint lost `(4,1)` after `UPDATE edges SET b=1 WHERE a=5` | `rowid>lo` marked new members, but SQLite reuses rowids once the newest rows are deleted, which the delete pass does | `tests/4_features.rs` `row1_binary_closure_with_cycles` | member and work tables carry `INTEGER PRIMARY KEY AUTOINCREMENT`, storage format 5; recursive views below it must be re-created |
| generation-gated `refresh` skipped the retry of a failed connect-time bind | generation stamped before the bind succeeded | `tests/3_relational.rs` `narrow_source_column_order_changes_are_transactional` | stamp lands after the bind in `Table::refresh` |
| `vm_step` per statement read as thousands for a three-value insert | `sqlite3_stmt_status` is cumulative per handle; `StmtRef` in the trace callback cannot reset it; one handle per view connection shares SQL text | none: measurement tool | `scripts/statement-costs.sh` diffs by `run` within (view, sql); `run=1` is a new handle |
| statement `nanos` read as 0 or 1000000 | the SQLite build's profile clock has 1ms granularity | none: measurement tool | the ms column is a sample count; time by node kind comes from span `time.busy` or a sampler |
| 18% of samples in `mach_vm_allocate`/`memset`/`vm_deallocate` | Apple's `libsqlite3.dylib` backs every ephemeral b-tree (IN-subquery, DISTINCT, GROUP BY, recursive CTE) with a purgeable page cache | `samply record` on the `8_group_limit` binary | `bundled` SQLite is the default feature; extension builds pass `--no-default-features` |
| three "improvements" reported from 1ms-quantized time columns and overlapping run spreads | statement counts and a noise column stood in for time | none | a change counts only when three runs all fall below the previous three; time comes from spans or a sampler |
