# Brief: ivm-duckdb — a DuckDB + OpenIVM implementation of the Engine trait

User (2026-10-05): "yolo ... adding a new impl for duckdb and its openivm plugin for its version it
works with and see what happens as a quick swap with the most generic sql client tooling we can,
i dont want smart clients, if sql then sql so it should be turnkey."
Standing rule (2026-10-03): do not link DuckDB into the Rust binaries.

## Today

- Trait: `crates/ivm-engine/src/1_rel.rs:93` `Engine` (install, settle, counters, snapshot,
  intern_*, text, any_value, rewinds/mark/rewind). IR: `crates/ivm-ir`.
- Implementations: `crates/ivm-dd` (default engine in sprefa now), `crates/ivm-sqlite` (paused;
  its SQL generation in `crates/ivm-sqlite/src/3_nodes.rs`, `catalog.rs` is the closest reference
  for lowering IR to SQL).
- Shared conformance: the ivm-dd / ivm-sqlite test scripts and corpus under `crates/*/tests`
  (`tests/corpus/`, scripts run on both engines and compare rows).
- OpenIVM checkout: `~/projects/openivm` (commit `cec35ae`; `duckdb` submodule pins the DuckDB
  version; not built yet). Earlier shootout brief: `plans/2026-10-03-duckdb-vs-dd.brief.md`.
- sprefa consumes engines through `Backend` in `sprefa .../src/_6_eval/_9b_ir_eval.rs` (`DL8_ENGINE`).

## Do

1. Candidate table first (`plans/2026-10-05-ivm-duckdb-engine.findings.md`): generic transports that
   do not link DuckDB into the binary: the `duckdb` CLI as a child process speaking plain SQL on
   stdin and CSV/JSON on stdout; ADBC driver manager loading the DuckDB driver at runtime; ODBC.
   Pick the plainest one that keeps it "SQL in, rows out". Build OpenIVM and its matching DuckDB
   (the submodule's version) and record versions.
2. New crate `crates/ivm-duckdb` implementing `Engine`: install = SQL DDL for sources and
   OpenIVM materialized views per IR node/stratum (or plain views + recompute where OpenIVM
   cannot express an operator: say which, logged); settle = insert/delete into base tables,
   `PRAGMA refresh` (or OpenIVM's equivalent), read deltas. Interning (terms, text) through
   tables. Recursion (LetRec): state what DuckDB/OpenIVM supports (recursive CTEs; OpenIVM
   coverage) and fall back to recompute with a log line.
3. Run the shared conformance corpus on ivm-duckdb vs ivm-dd: pass/fail per case, every row
   difference listed. Then wire `DL8_ENGINE=ir:duckdb` in sprefa (separate small commit, sprefa
   branch) and run the sprefa suite with it; report pass/fail counts and the first failures.
4. Work counts, no wall clock: statements sent, rows in/out per settle, refreshes, for the
   conformance corpus and (if it runs) the registry build.

## Laws

- No DuckDB crate or library linked into any Rust crate's build; the engine talks to an external
  DuckDB through the chosen generic transport.
- `hafley-rs` in this repo is a gitignored symlink; in the worktree create
  `hafley-rs -> /Users/chrishafley/projects/sprefa-wt/hafley-rs-dep`.
- Do not change `ivm-ir` or the `Engine` trait unless required; if required, keep ivm-dd and
  ivm-sqlite compiling and passing, and list each change.
- One other lane runs on this machine; `-j 4`. Features in Rust/SQL only. No caching. No Python.
- Never `git commit -n`, no merge, no push.

## Receipt

```
status: done | blocked
sqlite_ivm branch + sha:
sprefa branch + sha (DL8_ENGINE=ir:duckdb wiring):
transport + versions (DuckDB, OpenIVM):
conformance: <passed/failed of N vs ivm-dd; first differences>
sprefa suite with ir:duckdb: <passed/failed>
operators that fall back to recompute: <list>
next: <one action>
```
