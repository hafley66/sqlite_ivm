# SQL-facing feature acceptance, 2026-09-09

Version 0.2.0, storage format 2. DDL is excluded from this feature comparison.
The accepted SQL grammar and its boundaries are listed in [the component README](../README.md#supported-query-shapes).

The tested native library has SHA-256
`b258bec9b25316c7fa0f62d773dd0a72ea286429dfe1048d005489e4a13e9de4`.
The manifest fingerprints the extension, compiler, operator implementation,
fixtures, native consumer, DD graphs, runners, tests and CI workflow.

## Current results

| Execution | Cases | Checked states | Result |
|---|---:|---:|---|
| Loaded SQLite IVM | 44 | 7,656 | Passed |
| Independent DD graphs | 44 | 7,656 | Passed |
| Separate SQLite connection without extension | 44 | 7,656 | Passed; independently applies source mutations |
| Ordinary PostgreSQL queries | 44 | 7,656 | Passed |
| pg_ivm 1.15 admitted views | 17 | 2,958 | 16 cases match; FULL JOIN USING mismatches in 168 states |
| pg_ivm rejected definitions | 27 | 0 | Recorded as unsupported |

The shared feature fixture contains 174 states per case: empty input, duplicates,
key/value moves on each side, removal of last support, NULLs, empty groups,
transactions, nested savepoints, rollback and deterministic generated mutations.
Every engine checks source rows as well as output bags. DD applies signed row
updates, advances every input frontier, and waits for its output probe. A held
input frontier and an intentionally incorrect expected count are checked.

Additional verification passed:

- 42 Rust integration tests.
- Nine tests through the loaded release library, including the 44-case fixture,
  BLOB/IEEE values, collations, recursive NULL support, output affinity, source
  triggers/generated columns/cascades, deterministic functions, WAL snapshots,
  writer contention and injected maintenance failure.
- Six native Bash CRUD/lifecycle scenarios.
- The original 20 circuit families, each with 13 states, through the loaded
  library and reopen/rename/rollback/drop checks.
- A fresh 400-row circuit comparison: 260 mutation checks per arm for SQLite IVM,
  DD and ordinary SQLite, with no failed or unsupported circuit arms.

The additional BLOB, floating, collation, function and concurrency probes use
SQLite's query semantics as their oracle. They are separate from the typed DD
fixture above. Floating aggregation may depend on accumulation order; these
results do not establish bit-identical floating sums across different engines.

## Implemented changes

- Symmetric RIGHT/FULL joins, CROSS and non-equality joins, residual ON predicates,
  USING/NATURAL keys, qualified stars and parenthesized join scopes.
- Filtered and joined EXISTS/NOT EXISTS; nullable COUNT/SUM routing.
- CASE/CAST, deterministic registered scalars, aggregate expressions, FILTER,
  HAVING and GROUP BY aliases/ordinals.
- DISTINCT/group/compound/window top-k, OFFSET, aliases and explicit NULL order.
- Multiple window functions, frames and named windows; composition through FROM
  subqueries; explicit CTE column names and shared consumers.
- Typed BLOB/REAL trigger transport, exact stored row identity, collation-aware
  operator keys, and public output affinity/collation for downstream queries.
- Arrangement-based source trigger/cascade maintenance, generated values,
  rollback/failure isolation and collated recursive deletion with alternate NULL
  support.

Maintenance emits row differences within affected keys, groups, partitions and
recursive regions. Defining SELECTs are evaluated for initial creation and by
test oracles. Source mutations do not rebuild the entire materialized view.

## Observed pg_ivm 1.15 mismatch

After the final matching right row is deleted from a FULL JOIN USING view, the
merged key becomes NULL in pg_ivm's maintained result. Ordinary PostgreSQL,
ordinary SQLite, SQLite IVM and DD preserve the left key.

| After deleting the right match | merged key | left key | right key |
|---|---:|---:|---|
| Defining SELECT / SQLite IVM / DD | 1 | 1 | NULL |
| pg_ivm 1.15 | NULL | 1 | NULL |

The [minimal SQL reproduction](43_pg_full_using.sql) was run independently on
PostgreSQL 18.6 with pg_ivm 1.15. The expanded fixture records its first mismatch
at state 6 and continued mismatches through state 173.

`13_feature_pg.sh` returns failure for this result mismatch. The separate
`15_pg_baseline.sh` regression gate requires the exact recorded behavior in
[44_pg_ivm_1_15_expected.json](44_pg_ivm_1_15_expected.json), including the wrong
rows, while requiring every ordinary PG query to pass. This does not classify
the mismatching pg_ivm case as a semantic pass.

PostgreSQL spellings explicitly match SQLite NULL ordering, the fixture's ASCII
LIKE behavior, HAVING aliases, and SQLite's merged USING-star projection. The
actual SQL variants are stored in the shared fixture. pg_ivm restrictions are
captured from native creation errors, rather than inferred from a feature label.

## Reproduce and inspect

```bash
export SQLITE3=/path/to/sqlite3
export SQLITE3_LIB_DIR=/path/to/sqlite/lib
export SQLITE3_INCLUDE_DIR=/path/to/sqlite/include
bash sqlite_ivm/scripts/9_verify.sh
bash sqlite_ivm/scripts/12_features.sh /absolute/new/features
bash sqlite_ivm/scripts/14_native_values.sh /absolute/path/libsqlite_ivm.dylib
IVM_POSTGRES_PREFIX=/path/to/postgres bash sqlite_ivm/scripts/13_feature_pg.sh /absolute/new/features
IVM_POSTGRES_PREFIX=/path/to/postgres bash sqlite_ivm/scripts/15_pg_baseline.sh /absolute/new/features
```

Local results:

- [Native receipt](results/features-020-final/native.jsonl).
- [DD receipt](results/features-020-final/dd.jsonl).
- [PostgreSQL and pg_ivm receipt](results/features-020-final/pg.jsonl).
- [Binary/source manifest](results/features-020-final/manifest.json).
- [Original-circuit comparison](results/features-020-final-circuits.jsonl).
- [Base verification log](results/features-020-final/verify.log).
- [Loaded-library value/transaction log](results/features-020-final/native-values.log).

The manifest SHA-256 is
`50ad530e7b3f8add27ef8b2d75e694398b48feda1d432309f41acf287b161ff9`.
Receipts and generated databases are retained locally and excluded from source
archives. The source archive includes generators, all tests, independent DD
graphs, the pg_ivm reproduction and the recorded baseline.

CI coverage adds Linux/macOS typed native/DD tests, native value and transaction
probes, and a PostgreSQL 16 / pg_ivm 1.15 comparison job. Remote CI has not run in
this local task. Local PostgreSQL verification used 18.6; SQLite was 3.53.2.

## Boundary of the claim

These checks cover the named SQL-facing operators and tested compositions.
Arbitrary recursive DD programs, custom DD Rust operators, arbitrary timestamp
partial orders, exposed frontiers and negative source bags are outside the
SQLite SQL API. User-defined SQL aggregates, custom collations and several SQL
spellings remain outside the accepted grammar. A universal claim of full pg_ivm
and DD semantics is therefore not established by this acceptance record.

The older performance observations in [39_acceptance.md](39_acceptance.md) belong
to version 0.1.0. This feature run makes no new performance ranking claim.

Primary pg_ivm reference: [1.15 release](https://github.com/sraoss/pg_ivm/releases/tag/v1.15)
and [supported definitions](https://github.com/sraoss/pg_ivm/blob/v1.15/README.md).
