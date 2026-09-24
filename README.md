# SQLite IVM

Transactional incremental SQL views for SQLite, implemented as a Rust loadable
extension. Ordinary source `INSERT`, `UPDATE`, and `DELETE` statements maintain
persistent results inside the source transaction. Rollback and savepoints cover
source rows, pending deltas, and results together.

Version 0.3.0 is a prerelease with the explicitly bounded SQL contract below.
MIT OR Apache-2.0 licensed. The package is independent of the surrounding compiler.

## Use

```sql
.load ./libsqlite_ivm
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TABLE farms(id INTEGER PRIMARY KEY, region TEXT, price INTEGER);
CREATE VIRTUAL TABLE earnings USING sqlite_ivm('
  SELECT region, COUNT(*) AS farms, SUM(price) AS dollars, AVG(price) AS mean
  FROM farms GROUP BY region
');
INSERT INTO farms VALUES(1,'north',20),(2,'north',30);
SELECT * FROM earnings;                       -- north | 2 | 50 | 25.0
UPDATE farms SET price=40 WHERE id=1;
SELECT * FROM earnings;                       -- north | 2 | 70 | 35.0
DELETE FROM farms WHERE id=2;
SELECT * FROM earnings;                       -- north | 1 | 40 | 40.0
ALTER TABLE earnings RENAME TO income;
DROP TABLE income;
```

Load the extension on every reader and writer connection. Writers must enable
both pragmas. Public result CRUD is rejected. Read results with ordinary SELECT;
row order requires an outer ORDER BY. General results preserve bag multiplicity.
The general virtual-table reader currently scans stored output rows.

## Supported query shapes

The same extension also registers an experimental frontier path for the ISO
engine. It has separate functions and a separate catalog:

```sql
CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
SELECT sqlite_ivm_frontier_install('team_cost',
  'SELECT team, count(*) AS jobs, sum(cost) AS total_cost FROM job GROUP BY team');
SELECT * FROM frontier_team_cost;
SELECT * FROM frontier_team_cost_delta; -- signed changes at the last frontier
SELECT sqlite_ivm_frontier_drop('team_cost');
```

The frontier path currently accepts integer projection, equality joins, and
`COUNT(*)`/`SUM(integer)` grouping for the ISO cases. It rejects `WHERE`,
`DISTINCT`, `HAVING`, `ORDER BY`, `LIMIT`, outer/cross/comma joins, `AVG`,
`MIN`/`MAX`, `COUNT(column)`, and NULL input cells. Existing
`CREATE VIRTUAL TABLE ... USING sqlite_ivm` views retain the broader contract
below. The two paths may read the same source table in one connection.

| Operator | Implemented contract |
|---|---|
| Projection and selection | Arithmetic and bit operations, comparisons, CASE, CAST, LIKE/GLOB, NULL tests, literal IN/BETWEEN, deterministic SQLite and registered scalar functions |
| Joins | INNER, LEFT, RIGHT, FULL, CROSS; ON, USING, NATURAL; comma joins and `JOIN` without ON take their equality keys from WHERE conjuncts; composite keys, residual predicates, non-equality conditions, self joins, parenthesized join trees |
| Existence | Correlated EXISTS / NOT EXISTS under AND, including filtered and joined subqueries |
| Aggregation | Composite groups and ordinals; COUNT, SUM, AVG, MIN, MAX; DISTINCT arguments, FILTER, aggregate expressions, HAVING, empty global aggregates |
| Sets | DISTINCT, UNION ALL, UNION, EXCEPT, INTERSECT |
| Top-k | Literal LIMIT/OFFSET, output aliases/ordinals, NULLS FIRST/LAST; composition with grouping, DISTINCT, compounds and windows |
| Windows | Multiple partitioned windows; ROW_NUMBER, RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST, NTILE, LAG/LEAD, FIRST/LAST/NTH_VALUE, aggregate windows, frames and named windows |
| Composition | FROM subqueries, CTEs with explicit column names, shared CTE consumers |
| Recursion | Positive `WITH RECURSIVE` strata: n-ary heads, several anchors, several recursive UNION distinct terms, inner and comma joins over sources, CTEs and subqueries, WHERE, DISTINCT, expression heads; consumers (aggregate, DISTINCT, EXISTS, NOT EXISTS, later recursive CTEs) are later strata; delete-and-rederive with semi-naive rowid-range rounds; cyclic insertion/deletion, NULL members and matching key collations |
| Values and collations | NULL, conforming INTEGER/REAL/TEXT/BLOB values; BINARY, NOCASE, RTRIM; adjacent IEEE floating values, infinities and empty BLOBs survive trigger transport |
| Transaction effects | Source triggers, generated columns, foreign-key cascades, savepoint rollback, writer contention, reader snapshots and maintenance failure rollback |

Acceptance includes the original 20 circuit families and 51 additional query
combinations, each with 174 source states. The native extension and independent
DD graphs are compared with a separate SQLite connection that has no extension.
Ordinary PostgreSQL queries provide another comparison. Exact results and the
pg_ivm 1.15 mismatch are recorded in the bench harness receipts
(`plans/costs/shootout-rust.md`).

Sources must be ordinary `main` tables. Values conform to declared affinities;
BLOB storage uses columns with BLOB or no declared affinity. Load deterministic
registered functions on every connection and keep their definitions and external
dependencies stable. Datetime functions and volatile functions are rejected.

Accumulator overflow aborts the source statement.
Floating aggregation follows SQLite arithmetic; accumulation order can affect
rounding. ORDER BY ties retain SQL's unspecified tie order. Equality keys normalize
numeric equality; stored row identities preserve types and floating-point bits.

The accepted grammar has explicit boundaries. Aggregate/window composition and
expressions wrapping window calls require a FROM subquery. Window inheritance,
scalar subqueries, IN subqueries, aggregate EXISTS, EXISTS under OR, custom
collations, custom aggregates, bind parameters and queries without FROM are
unsupported. Inside a recursive step, aggregation, EXISTS, NOT EXISTS or IN over
the step's own relation is rejected as `recursive step may not aggregate or
negate its own relation`; `UNION ALL` recursion is rejected as `recursive UNION
ALL unsupported`; outer joins, USING, ORDER BY and LIMIT inside the recursive
CTE are rejected. SQLite 3.53 itself rejects mutual recursion between two CTEs
(`circular reference`) and a second reference to the recursive table in one
step (`multiple references to recursive table`), so "mutual" recursion is
spelled as one CTE with a discriminator column, for example `parity(node,odd)`.
A recursive CTE may consume an earlier recursive CTE; a fixpoint nested inside
another step is not spellable in SQLite and therefore not built. DD comparison uses signed row deltas and
sequential u64 epochs; the SQLite API exposes transactions and relational bags.
It does not expose arbitrary DD timestamps, frontiers, negative source bags,
custom Rust operators or durable DD execution.

## Source DDL

Use the managed source DDL functions so defining queries and ownership metadata
change in the same transaction:

```sql
SELECT sqlite_ivm_rename_source('farms','growers');
SELECT sqlite_ivm_rename_column('growers','price','cost');
SELECT sqlite_ivm_drop_source('growers',0); -- rejects managed dependents
SELECT sqlite_ivm_drop_source('growers',1); -- explicitly drops dependents too
```

Rename uses SQLite's own ALTER TABLE rewriting, regenerates source hooks, and
preserves result and intermediate rows. Public result column names stay fixed.
The functions participate in caller transactions and roll back atomically.
Direct source ALTER/DROP, ADD/DROP COLUMN, and source type changes are outside
this DDL protocol. Result CREATE/ALTER RENAME/DROP use ordinary SQLite DDL.
Compatibility functions `sqlite_ivm_create(name,query)` and `sqlite_ivm_drop(name)`
remain available.

## State and incremental work

`xCreate` creates native source indexes, result storage, source triggers, key
columns, and an ownership manifest. Operator inputs are read from authoritative
source tables and compiled subqueries. Persistent copies of join/group input rows
are absent. `xConnect` reads a persisted result schema; maintenance rebinds after
managed source DDL.

```text
OLD / NEW cells -> signed batch -> native source-index probes
                                       |
                  deltaL JOIN newR + newL JOIN deltaR - deltaL JOIN deltaR
                                       |
                      group columns -> signed contributions
                                       |
                         result INTEGER PRIMARY KEY
                         update surviving groups in place
```

Join and aggregate keys use separate SQLite values and composite indexes.
NULL group keys match with `IS`; join equality uses `=`. Touched groups have
integer IDs in a dictionary with native key columns. Result rows have explicit
integer primary keys, and projected aggregate groups preserve those IDs across
updates. Result insertion/retraction and duplicate consolidation compare native
values, storage types, and exact REAL bits. An integer checksum supports the
existing corruption-recovery check; it never selects a row or defines equality.
Set and recursive membership keys retain their existing encoded representation.
UNION and DISTINCT keep one representative and signed support counts per
interned integer key in a persistent operator result table. They update that
table from child deltas; deleting the current representative while support
remains re-reads the affected source key. EXCEPT and INTERSECT retain the
affected-key before/after path. No per-input row copies are stored.
Temporary before-images share one table per row width and are cleared after
each operator, so view installation does not create one before table per node.

Projection/filtering and inner joins propagate signed deltas. Eligible bounded
integer COUNT/SUM groups add contributions and maintain non-null support counts.
Other aggregates, outer joins, EXCEPT, INTERSECT, windows, and top-k evaluate affected-key
before/after results and emit their signed difference. Before-input reads subtract
the pending delta from current source rows. A recursive CTE retains member/work
state and uses delete-and-rederive; its work table is cleared after the drain.
Large affected groups and graph regions can require large work. The batch boundary
is a transaction/read drain; this API has no dataflow timestamp frontier runtime.

`xRename` preserves state and index B-trees; `xDestroy` validates owned DDL before
cleanup. Shared `__ivm_*` catalogs remain after the last view is dropped. New views
use storage format 11. Compatible formats 2 through 10 rebuild through the existing
writable-database migration path, removing copied input tables and installing the
native-key layout. Pre-5 recursive state still requires its matching older
extension; format 1 and the original ordinary-view prototypes are not migrated.

Enable `SQLITE_DBCONFIG_DEFENSIVE` to prevent direct shadow-table writes. Reserved
hidden columns (`__ivm_source`, `__ivm_adding`, `__ivm_row`) and catalogs are internal
maintenance interfaces, not a security boundary. Direct internal commands and
catalog edits are outside the supported API. No persistent triggers are installed
on shadow tables, which is required for reopening on SQLite 3.53.2.

Maintenance emits `tracing` spans through `hafley-observe`, installed once on the
first `register` (extension load included) unless the host process already owns a
global subscriber. The default filter is `warn`, so nothing prints until
`RUST_LOG=sqlite_ivm=debug` (or another `RUST_LOG` filter) asks for it;
`HAFLEY_LOG_FORMAT=json` switches the format. Spans are `maintain` (view, source
table, sign), `fixpoint` (view, node, rows in, rounds) and `round` (phase, index,
rows written). Fields carry names and integers, never row contents.

## Batch maintenance

`sqlite-bulk-trigger` collects source row changes with savepoint marks. A drain
seeds signed rows into temporary input tables, then walks the plan in dependency
order. The number of inserted rows determines which consumers need work.

Inner joins execute `delta_left JOIN old_right`, update the left arrangement,
then execute `new_left JOIN delta_right` and update the right arrangement.
Outer joins, sets, groups, and windows compare results before and after updating
the affected keys. Recursive operators use their existing semi-naive rounds.
Output deltas retract and insert stored results in the same transaction.
New arrangement indexes combine row hashes with exact identity for one UPSERT;
older hash-only indexes retain the exact-identity UPDATE/INSERT path.

Terminal COUNT(*)/SUM groups with projected integer keys can reuse indexed stored
before-images and add signed integer contributions. Nullable sums retain non-null
support counts in a shadow table. Eligibility bounds each scalar contribution to
an absolute value of 1,000,000 and conservatively bounds support plus the incoming
absolute delta to 1,000,000. These bounds keep intermediate integer arithmetic
exact. Groups leaving this domain retain affected-group recomputation thereafter.
Other aggregates, HAVING, windows, and LIMIT retain their existing paths.

`hafley-observe::CountRecorder` supplies numeric event samples and aggregates.
SQL phase names, workloads, cost fixtures, and regression assertions live in this
repository. Both shared crates are local path dependencies in the sibling
`../hafley-rs/crates/` directory: `hafley-observe` and `sqlite-bulk-trigger`.
Library changes are made there and used directly by the plugin and benchmark.

## Build, test, package

Run from this repository:

```bash
cargo fetch --locked
cargo fetch --locked --manifest-path bench/Cargo.toml
just verify
just shootout smoke
just crossover
just crossover-observe
just package
```

`just verify` runs the root correctness and timing gate, three native extension
loading tests, the CRUD shell scenario, and the CLI compass. On macOS it selects
Homebrew SQLite when `SQLITE3` is unset. `cargo-nextest`, Python 3, and coreutils
`timeout` or `gtimeout` are required. The crossover also requires Node.js.

The native extension builds into `target/extension`, separately from linked Rust
tests and benchmark binaries. `just package` writes binary and source archives
plus SHA-256 sidecars into `dist`. The source archive contains committed HEAD;
building it requires the sibling `hafley-rs` checkout with those shared crates.

## Shared benchmark

See [bench/README.md](bench/README.md) for reproducible commands, source provenance,
timing boundaries, receipts, and measured limits. The binary runs the circuits
shootout (`shootout`), the scale sweep (`scale`), and fixture dumps
(`dump-fixture`) against the built extension. Unsupported pg_ivm families are
reported explicitly. SQLite and PostgreSQL are durable; DD is volatile.

## Reading order

1. `src/0a_catalog.rs`: ownership manifest and cleanup.
2. `src/0b_relational.rs` through `src/0f_columns.rs`: query compilation and expressions.
3. `src/1a_relational.rs` and `src/1b_state.rs`: persistent arrangements and keys.
4. `src/1c_materialize.rs`: SQL for initial and affected-key results.
5. `src/1d_drain.rs` and `src/1e_program.rs`: batch execution and prepared SQL.
6. `src/2_vtab.rs` and `src/2a_source_ddl.rs`: lifecycle, reads, managed DDL.
7. `src/3_extension.rs`: SQL functions and extension ABI.

rusqlite 0.40.2 and sqlite3-parser 0.17.0 are pinned in Cargo.lock. Rusqlite owns
the virtual-table adapters. A narrow ABI descriptor adapter adds xRename and
xShadowName through its public repr(transparent) Module layout; recheck that
layout before upgrading. No C source is required.

## Combined engine report

```bash
IVM_POSTGRES_PREFIX=<postgres prefix> target/release/bench shootout quick
```

Runs the 20 shared circuits across the native extension, PostgreSQL/pg_ivm,
SQLite queries, and DD, with a per-engine report, JSONL receipts, and explicit
`n/a (<reason>)` markers for unsupported definitions. See
[bench/README.md](bench/README.md) for dependencies, profiles, artifacts, and
exit codes.

## Native plugin tracing

The native extension and linked library retain the same statement instrumentation.
Use the existing hafley-observe JSON formatter and runtime filter:

```bash
cd /Users/chrishafley/projects/sprefa-wt/sqlite-perf-main
RUST_LOG='dl8=debug,sqlite_ivm=trace,sqlite=trace' HAFLEY_LOG_FORMAT=json HAFLEY_TRACE='' DL8_ENGINE=sqlite SQLITE_IVM_LIB=/Users/chrishafley/projects/sqlite_ivm/target/extension/release/libsqlite_ivm.dylib target/debug/dl8 compile fixtures/aggregates/0_sum.dl7 > /private/tmp/ivm-compile.stdout 2> /private/tmp/ivm-compile.jsonl
```

`prepare_start` records the full SQL before preparation. Its `prepare` span
carries SQL bytes and cache API path; `prepare_end` records success or error.
The separate `execute` span records start, completion, returned row count when
known, and errors. A native regression test checks that preparation events are
present in the loaded release extension. `cached=true` means `prepare_cached`
was called; it does not assert that the cache contained that statement.

Function entry/exit spans include source file and line at TRACE level. Explicit
loop events and operator spans expose population, maintenance, unchanged-input
skips, and emitted delta counts. Population records the operator ID, kind, input
IDs and width. Cursor prepare/step calls are also recorded. Batch execution,
pragma updates and collector guards expose combined start/end events because
those library APIs own their internal preparation. Tracing does not separately
record every physical source line or iterator-adapter invocation.

`RUST_LOG` takes precedence over `HAFLEY_LOG`. Full TRACE includes expanded SQL
and can generate large files. `sqlite_ivm=debug,sqlite=debug` retains statement
preparation and operator attribution without function and loop TRACE events.
The `statements` Cargo feature remains accepted for existing recipes; removing
it no longer compiles native observability away. Set `RUST_LOG=off` for timings
without tracing. Diagnostic log intervals include recording overhead.
