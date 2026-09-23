# Two engines, one settled frontier

## Claim and scope

A single typed frontier API can run the access join plus union and the grouped count plus sum through an in-process Rust store or a SQLite extension callback. This lab tests the two shapes in `plans/engine-iso`; it does not implement arbitrary relational plans, recursion, or cross-IVM composition.

Source revision: `sqlite_ivm` `81d01c9` (input packet). The correlated `hafley-rs` checkout used for `hafley-observe` and `sqlite-ext` was `91b41b47`. Dependencies are pinned in `Cargo.lock`, including `rusqlite 0.40.2`. The lab is its own Cargo workspace and has no dependency on the `sqlite_ivm` or Sprefa crates.

## Type signatures

```rust
trait FrontierEngine {
    type Plan;
    type Change;
    type Output;
    type Error: std::error::Error;

    fn install(&mut self, plan: Self::Plan) -> Result<(), Self::Error>;
    fn apply(&mut self, changes: &[Self::Change]) -> Result<Frontier, Self::Error>;
    fn snapshot(&self) -> Result<Vec<Self::Output>, Self::Error>;
    fn teardown(&mut self) -> Result<(), Self::Error>;
}
```

`Plan` is either `JoinUnion` or `GroupCountSum`; both implementations currently use the same `Plan`, `Change`, `Vec<i64>` output, and `EngineError` types. The associated types mark the backend boundary, but this lab has no second concrete representation for any one of them. `SourceRow { id: i64, cells: Vec<i64> }` gives each row a stable integer identity. `Change` carries a source ID and signed weight; `Frontier` carries a sequence ID and net signed output rows. The same four methods are called for both plan variants in `tests/0_contract.rs`.

## Instance lifetime and effects

```text
new RustEngine / open SqliteEngine
  install(plan)       create arrangements and output state
  apply(changes)      validate, apply one atomic source batch, settle, return net output
  snapshot()          read visible output after that frontier
  apply(changes) ...
  teardown()          release installed state
```

The Rust instance owns maps and indexed join arrangements until teardown. The SQLite instance owns its connection; `install_on(&Connection, Plan)` also installs the same extension logic on a caller-owned connection. One `apply` call maps to one SQLite transaction, whose watch collector invokes maintenance at commit. `register_native_fixture` exposes two installation functions in a loadable extension used by `tests/2_native.rs`.

## Storage, reads, and writes

Rust source rows are keyed by integer row ID. A join-key index selects matching rows; signed join work uses `ΔL × R_old + L_new × ΔR`. A map of output support weights emits a visible row only across support zero. Group state stores count and sum by integer group ID.

SQLite stores each source in `src_N(id INTEGER PRIMARY KEY,c0 INTEGER,c1 INTEGER)`, collects signed rows in `__iso_dN`, and indexes join columns. The join maintenance SQL computes `ΔL × R_new + L_new × ΔR - ΔL × ΔR`, then adds direct grants and groups signed support by output integers. `__iso_support` and `__iso_groups` hold output state; `__iso_outbox` returns only net visible changes. The callback clears delta tables after a settled frontier. The aggregate path groups signed count and sum contributions. The schema has one output-state table for the installed plan, with no row serialization, JSON payload, or string join key.

## Correctness and measurement commands

```bash
cd /Users/chrishafley/projects/sqlite_ivm/plans/engine-iso
sqlite3 -batch -noheader -separator $'\t' :memory: < 2_oracle.sql > /tmp/engine-iso-actual.tsv
diff -u 3_expected.tsv /tmp/engine-iso-actual.tsv
sqlite3 -batch -noheader -separator $'\t' :memory: < 3b_aggregate.sql > /tmp/engine-iso-aggregate-actual.tsv
diff -u 3c_aggregate_expected.tsv /tmp/engine-iso-aggregate-actual.tsv

cd /Users/chrishafley/projects/sqlite_ivm/labs/20260923.0.the-gang-routes-frontiers-through-trait-engines
SQLITE3_LIB_DIR=/opt/homebrew/opt/sqlite/lib SQLITE3_INCLUDE_DIR=/opt/homebrew/opt/sqlite/include cargo test --offline --quiet
cargo run --offline --example 0_run -- rust
cargo run --offline --example 0_run -- sqlite
ENGINE_ISO_DB=/private/tmp/engine-iso-direct-case.db cargo run --offline --example 0_run -- sqlite
```

`cargo test` passed: four oracle contract tests, three SQLite lifecycle tests, and two native extension tests. The contract tests compare every expected snapshot and signed delta, including empty frontiers, support 2→1 and 1→0, a simultaneous join-input cross-term, and aggregate empty and net-zero batches. The lifecycle tests cover savepoint and transaction rollback, callback failure with the previous snapshot retained, and bounded schema object counts. The native tests load the compiled fixture through SQLite and exercise both plans. The standalone oracle scripts are input fixtures; the Rust tests parse their expected TSVs.

One direct example invocation after the final SQL refactor produced:

| Backend | Input rows | Net output rows | Snapshot | Elapsed apply | Process peak RSS |
| --- | ---: | ---: | --- | ---: | ---: |
| Rust | 5 | 2 | `[[1,100],[3,300]]` | 1,873,958 ns | 4,620,288 B |
| SQLite in-memory | 5 | 2 | `[[1,100],[3,300]]` | 535,083 ns | 6,897,664 B |

These are single invocations with different process startup conditions, not a paired throughput result. The in-memory SQLite run reported 11 tables, 5 indexes, 9 triggers, a 419-byte join-delta statement, SQLite allocator 224,176 B current and 277,168 B peak. The file-backed run reported a 4,096 B database and 230,752 B WAL before checkpoint, SQLite allocator 225,024 B current and 307,936 B peak, process peak RSS 7,749,632 B. Its `EXPLAIN QUERY PLAN` searches the source and delta join-key indexes, scans the direct delta, and uses a temp B-tree for the final group by. The collector reported 1 begin, 5 updates, 1 sync, and 1 commit for the five-input frontier.

`observations/1_sqlite.jsonl` captures an earlier debug run. Its SQLite PROFILE event for the join statement reported `vm_step=262`, `fullscan_step=3`, `sort=1`, `autoindex=0`, and `reprepare=0`; that event predates the conditional output-table refactor. The currently printed `maintenance_sql()` and `explain_maintenance()` are a repeatable way to inspect the final statement and its plan. No repeated-frontier timing or prepare-count series has been recorded yet.

## Limits and follow-up gates

The source schema is fixed at two integer cells per row, and the plan enum has two variants. The API has no dynamic program update, no recursive frontier, and no composed IVM graph. The Rust engine keeps temporary row copies while building pending changes. The SQLite path uses a temp B-tree in support aggregation. Neither path has a repeated-frontier performance series or a measured compiler workload.

The packet asked for `#[hafley_observe::test(timeout = "2s")]`; that macro is absent from the correlated `hafley-observe` checkout, so these integration tests use `#[test]`. The source revision and the missing macro are recorded rather than treating the timeout gate as passed.

## Verdict

The two bounded shapes satisfy the tested frontier and lifecycle contract. This lab does not establish a production backend boundary or performance parity with differential dataflow.
