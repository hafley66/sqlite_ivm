# Brief: DuckDB (+ OpenIVM) against dd and sqlite_ivm

User (2026-10-03): "we want both bc now we are competing duck with dd". "dont link duckdb into my
rust binaries".

## Rules

- No DuckDB in any Rust crate or Cargo.toml. No Python (user, 2026-10-03). Use the existing
  harnesses: sprefa `v6/sprefa-store/bench/run.sh` (engine = `bench/engines/<n>_<name>.sh`, shared
  CSV schema, gnuplot report) and sqlite_ivm `bench/` (`frontier-stress`, `scale`; an
  external-process arm the way `pg_ivm`/`pg_query` arms run). DuckDB runs through the `duckdb` CLI
  (`/opt/homebrew/bin/duckdb`, v1.5.5) with SQL files; scripts are shell or `.mjs`. Plus the OpenIVM extension file built from `~/projects/openivm`
  (`build/release/extension/openivm/openivm.duckdb_extension`). The DuckDB version that loads the
  extension must match the version OpenIVM pins (its `duckdb` submodule); use that build's own
  `build/release/duckdb` binary if the Homebrew one does not match.
- New files: engine scripts in the two harnesses, numbered; results where each harness writes them.
- Inventory of existing benches: `sqlite_ivm/plans/costs/0_benchmark_inventory.md`; add the new arms there.
- Every DuckDB result is checked against dd's rows for the same input; a mismatch fails the run.

## Part A: incremental (OpenIVM)

Workloads: the `frontier-stress` access (join under set UNION) and group (COUNT/SUM) shapes
(`bench/README.md`, `bench/frontier`). Export each generated frontier stream to SQL/CSV once; DuckDB
arm: base tables + `CREATE MATERIALIZED VIEW ... AS <shape>`; per frontier: apply inserts/deletes,
`PRAGMA refresh('<view>')`, read the view. Measure write+refresh time per frontier, read time, peak
RSS, same sizes as the existing arms. Compare with `sqlite-ivm`, `dd`, `direct-sqlite` from the
existing harness on the same stream. Also a DuckDB full-recompute arm (no OpenIVM).

## Part B: recursive batch (dl8 comptime programs)

Workloads: the c15 IR (`crates/ivm-sqlite/tests/corpus/8_c15_program.json`) and the IR of the dl8
`registry.rs` build (dump it from dl8 if a dump flag exists; else from `_9a_ir_lower.rs` output via
a debug env var already in the code; do not add Rust code without stopping to report). Write a
`.mjs` exporter: IR JSON -> DuckDB SQL, one table per relation, strata in order, `WITH RECURSIVE`
(or DuckDB `USING KEY`) for LetRec strata, plain `INSERT ... SELECT` otherwise. Term constructors
(Mint) become dictionary tables with integer ids. Compare total time and RSS against ir:dd and
ir:sqlite on the same program; rows must match.

If a construct cannot be expressed in DuckDB SQL, list it with the count of nodes affected and skip
that stratum (report it; no silent drop).

## Laws

- One heavy build at a time on the machine (`uptime`, `pgrep -f 'cargo|rustc|ninja|make'`); the
  OpenIVM C++ build is heavy: run it alone, `-j` at most 8.
- No edits to Rust product sources (dl8 `src/`, sqlite_ivm `crates/`); the sqlite_ivm `bench/` crate may gain an arm that shells out to the `duckdb` CLI (no duckdb crate). Commit only the new engine scripts and arms, results,
  the inventory rows, and a findings file `plans/2026-10-03-duckdb-vs-dd.findings.md`.
  No merge, no push. End commit messages with
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Receipt

```
status: done | blocked
branch + sha:
openivm build: duckdb version, extension path, build time
part A table: shape x size x arm -> write+refresh ms/frontier, read ms, RSS, rows match
part B table: program x arm -> total s, RSS, rows match, skipped strata
next: <one action>
```
