# SQLite IVM

Transactional incremental SQL views for SQLite, implemented as a Rust loadable
extension. Ordinary source `INSERT`, `UPDATE`, and `DELETE` statements maintain
persistent results inside the source transaction. Rollback and savepoints cover
source rows, intermediate arrangements, and results together.

Version 0.2.0 is a prerelease with the explicitly bounded SQL contract below.
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
The specialized integer grouping path also indexes group-key equality reads.
The general virtual-table reader currently scans stored output rows.

## Supported query shapes

| Operator | Implemented contract |
|---|---|
| Projection and selection | Arithmetic and bit operations, comparisons, CASE, CAST, LIKE/GLOB, NULL tests, literal IN/BETWEEN, deterministic SQLite and registered scalar functions |
| Joins | INNER, LEFT, RIGHT, FULL, CROSS; ON, USING, NATURAL; composite keys, residual predicates, non-equality conditions, self joins, parenthesized join trees |
| Existence | Correlated EXISTS / NOT EXISTS under AND, including filtered and joined subqueries |
| Aggregation | Composite groups and ordinals; COUNT, SUM, AVG, MIN, MAX; DISTINCT arguments, FILTER, aggregate expressions, HAVING, empty global aggregates |
| Sets | DISTINCT, UNION ALL, UNION, EXCEPT, INTERSECT |
| Top-k | Literal LIMIT/OFFSET, output aliases/ordinals, NULLS FIRST/LAST; composition with grouping, DISTINCT, compounds and windows |
| Windows | Multiple partitioned windows; ROW_NUMBER, RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST, NTILE, LAG/LEAD, FIRST/LAST/NTH_VALUE, aggregate windows, frames and named windows |
| Composition | FROM subqueries, CTEs with explicit column names, shared CTE consumers |
| Recursion | Unary reachability: anchor UNION distinct one recursive equijoin edge step; cyclic insertion/deletion, NULL endpoints and matching key collations |
| Values and collations | NULL, conforming INTEGER/REAL/TEXT/BLOB values; BINARY, NOCASE, RTRIM; adjacent IEEE floating values, infinities and empty BLOBs survive trigger transport |
| Transaction effects | Source triggers, generated columns, foreign-key cascades, savepoint rollback, writer contention, reader snapshots and maintenance failure rollback |

Acceptance includes the original 20 circuit families and 44 additional query
combinations, each with 174 source states. The native extension and independent
DD graphs are compared with a separate SQLite connection that has no extension.
Ordinary PostgreSQL queries provide another comparison. Exact results and the
pg_ivm 1.15 mismatch are recorded in [feature acceptance](bench/46_feature_acceptance.md).

Sources must be ordinary `main` tables. Values conform to declared affinities;
BLOB storage uses columns with BLOB or no declared affinity. Load deterministic
registered functions on every connection and keep their definitions and external
dependencies stable. Datetime functions and volatile functions are rejected.

The integer COUNT/SUM fast path is selected only when used columns are guaranteed
non-NULL and sources have no cascades or user triggers. Nullable queries use
relational arrangements. Accumulator overflow aborts the source statement.
Floating aggregation follows SQLite arithmetic; accumulation order can affect
rounding. ORDER BY ties retain SQL's unspecified tie order. Equality keys normalize
numeric equality; stored row identities preserve types and floating-point bits.

The accepted grammar has explicit boundaries. Aggregate/window composition and
expressions wrapping window calls require a FROM subquery. Window inheritance,
scalar subqueries, IN subqueries, aggregate EXISTS, EXISTS under OR, arbitrary
recursive programs, custom collations, custom aggregates, bind parameters and
queries without FROM are unsupported. DD comparison uses signed row deltas and
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

`xCreate` creates persistent operator arrangements, indexes, source triggers, and
an ownership manifest, then loads existing source rows once. `xConnect` reads a
persisted result schema. Maintenance lazily binds the catalog query and refreshes
it after managed source DDL; reconnect does not populate results again.

Source hooks send OLD/NEW row images to `xUpdate`. Projection and filtering emit
signed rows. Joins look up matching keys. Sets maintain support counts. Aggregates
and windows emit the difference within the affected group. Top-k reads an indexed
candidate prefix. Recursive deletion removes and rederives the affected reachable
region, then emits its net difference. No mutation rebuilds the entire view from
its defining SELECT. Large affected groups or graph regions can still require
large work.

`xRename` preserves state and index B-trees; `xDestroy` validates the exact owned
DDL before cleanup. Shared `__ivm_*` catalogs remain after the last view is dropped.
Storage format 2 records the new operator layouts and typed row identities.
Databases containing format 1 views require the matching 0.1.x extension; 0.2.0
rejects incompatible state before maintenance. Automatic format migration is
unavailable. The original ordinary-view prototypes are not migrated.

Enable `SQLITE_DBCONFIG_DEFENSIVE` to prevent direct shadow-table writes. Reserved
hidden columns (`__ivm_source`, `__ivm_adding`, `__ivm_row`) and catalogs are internal
maintenance interfaces, not a security boundary. Direct internal commands and
catalog edits are outside the supported API. No persistent triggers are installed
on shadow tables, which is required for reopening on SQLite 3.53.2.

## Build, test, package

```bash
cargo fetch --locked --manifest-path sqlite_ivm/Cargo.toml
SQLITE3=sqlite3 bash sqlite_ivm/scripts/9_verify.sh
bash sqlite_ivm/scripts/10_package.sh
```

On macOS select a CLI with extension loading, for example Homebrew SQLite:

```bash
export SQLITE3="$(brew --prefix sqlite)/bin/sqlite3"
export SQLITE3_LIB_DIR="$(brew --prefix sqlite)/lib"
export SQLITE3_INCLUDE_DIR="$(brew --prefix sqlite)/include"
bash sqlite_ivm/scripts/9_verify.sh
```

The base gate runs Rust tests, six native Bash scenarios, and the original circuit
fixtures. The expanded feature gate and native value/transaction probes run with:

```bash
bash sqlite_ivm/scripts/12_features.sh /absolute/new/feature-artifacts
bash sqlite_ivm/scripts/14_native_values.sh /absolute/path/libsqlite_ivm.dylib
IVM_POSTGRES_PREFIX=/path/to/postgres \
  bash sqlite_ivm/scripts/13_feature_pg.sh /absolute/new/feature-artifacts
```

The raw PostgreSQL comparison returns failure on result mismatches. The separate
`15_pg_baseline.sh` gate checks the exact recorded pg_ivm 1.15 behavior, including
its FULL JOIN USING failure, while requiring every ordinary PG query to match.
The Linux/macOS workflow runs the expanded native/DD tests; a PostgreSQL job
checks the recorded 1.15 behavior. Remote CI execution has not been performed here.

The GitHub workflow builds and tests on Linux and macOS and uploads release
archives. Local execution is distinct from a remote CI run. Archives include the
native library, README, both licenses, and a SHA-256 sidecar. Source publication
or an external release requires selecting a destination.

## Shared benchmark

See [bench/README.md](bench/README.md) for reproducible commands, source provenance,
timing boundaries, receipts, and measured limits. The native SQLite consumer
loads the actual extension binary. PostgreSQL/pg_ivm and DD use the preserved
shared adapters, fixtures, and independent oracle. Unsupported pg_ivm families
are reported explicitly. SQLite and PostgreSQL are durable; DD is volatile.

## Reading order

1. `src/0_query.rs`: checked integer query binding.
2. `src/0a_catalog.rs`: ownership manifest and cleanup.
3. `src/0b_relational.rs`: general SELECT-to-operator compilation.
4. `src/1_maintenance.rs`: integer delta SQL and hooks.
5. `src/1a_relational.rs`: persistent relational operators.
6. `src/2_vtab.rs`: virtual-table lifecycle, scans, update dispatch.
7. `src/2a_source_ddl.rs`: transactional source DDL.
8. `src/3_extension.rs`: SQL functions and extension ABI.

rusqlite 0.40.2 and sqlite3-parser 0.17.0 are pinned in Cargo.lock. Rusqlite owns
the virtual-table adapters. A narrow ABI descriptor adapter adds xRename and
xShadowName through its public repr(transparent) Module layout; recheck that
layout before upgrading. No C source is required.
