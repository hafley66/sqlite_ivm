# Native feature acceptance and circuit benchmark

The current query-feature gate is described in [46_feature_acceptance.md](46_feature_acceptance.md).
It checks 44 combinations over 174 states against loaded SQLite IVM, independent
DD graphs, a separate SQLite connection, and PostgreSQL. Value, collation,
recursive deletion, trigger/cascade, failure and concurrency probes also run
through the loaded extension. DDL is outside this feature gate.

```bash
bash sqlite_ivm/scripts/12_features.sh /absolute/new/features
IVM_POSTGRES_PREFIX=/path/to/postgres bash sqlite_ivm/scripts/13_feature_pg.sh /absolute/new/features
# Strict comparison above reports pg_ivm 1.15's observed mismatch.
IVM_POSTGRES_PREFIX=/path/to/postgres bash sqlite_ivm/scripts/15_pg_baseline.sh /absolute/new/features
```

PostgreSQL SQL spells NULL ordering explicitly, uses ILIKE for the fixture's ASCII
LIKE case, expands HAVING aliases, and spells SQLite's merged USING-star columns
explicitly. Receipts distinguish a rejected pg_ivm definition from an admitted
view whose maintained rows disagree. The latter remains a mismatch.

The following timing harness retains the original 20 circuit families.
[39_acceptance.md](39_acceptance.md) contains historical 0.1.0 measurements and
cannot establish performance of the current binary.

The SQLite arm loads this project's compiled Rust extension through SQLite's
extension API. DD and pg_ivm execute the same fixed circuit families and the
same 13 mutation states. The original JavaScript oracle supplies expected bags
and SHA-256 hashes independently of SQL and the SQLite implementation.

## Reproduce

Build and verify the component first:

```bash
bash sqlite_ivm/scripts/9_verify.sh
```

For PostgreSQL comparison, install Node dependencies in `bench/shared` using its
lockfile (`npm ci --prefix sqlite_ivm/bench/shared`) and set `IVM_POSTGRES_PREFIX`
to PostgreSQL 18.6 with pg_ivm installed. The runner creates and destroys its own
cluster with Unix sockets and `listen_addresses=''`.

```bash
export IVM_POSTGRES_PREFIX=/path/to/postgres-18.6
export IVM_TEMP_FILE_LIMIT=128MB
bash sqlite_ivm/scripts/11_shootout.sh /absolute/new/400.jsonl \
  --circuit-grid 12k --circuit-cells 400:10:10 --warmups 1 --repetitions 5
```

The SQLite/DD comparison does not require PostgreSQL or npm dependencies:

```bash
IVM_ARMS=sqlite-plugin-delta,dd,sqlite-query \
  bash sqlite_ivm/scripts/11_shootout.sh /absolute/new/12000.jsonl \
  --circuit-grid 12k --circuit-cells 12000:10:10 --warmups 0 --repetitions 1
node sqlite_ivm/bench/38_report.mjs /absolute/new/400.jsonl /absolute/new/12000.jsonl
```

`--circuit-cells` selects existing shared grid cells as rows:batch:fanout. It does
not change fixture generation. `--circuits` selects named families. Receipts must
use new paths; the shell wrapper refuses overwrites. The driver builds the pinned
DD adapters before measurement and executes engine processes serially.

## Timing and acceptance

Each state times source mutations plus maintenance completion, then result
materialization. Exact source rows, expected output bags, SQL oracle checks, and
hashes are checked outside that interval. Native reopen/rename/rollback/drop
checks also run after timed states. A failed create, mutation, check, or lifecycle
operation makes the native consumer fail. SQLite cannot claim an unsupported
family as a pass.

The report separates initial loading, whole-source deletion, and the remaining
11 states. Whole-case totals include all 13 states. Engine order rotates between
measured repetitions. Fixture caching retains only the current case, outside
measurement, without changing its generator or contents.

SQLite uses WAL and synchronous FULL. PostgreSQL uses fsync, synchronous_commit,
and full_page_writes. DD is single-worker, volatile, and advances all input
frontiers before awaiting the output probe. Its scope here is sequential u64
epochs, not arbitrary partially ordered timestamps or durable recovery.

There is a 120-second process timeout and 20-minute overall deadline. Memory is
observed where process inspection is available; no cgroup memory cap is enforced.
Unavailable observations are null. PostgreSQL temporary-file limits are recorded
in metadata. These are local runs, without CPU isolation.

## Provenance

`shared/` preserves the circuit and semantic fixtures, PostgreSQL adapters,
DD implementations, and receipt protocol from the existing
`postgres_pglite_ivm` lab in worktree `feature/sqlite-ivm-astra`, whose HEAD was
`f7a7f937ffa5425edb5a30aec634ef5bbb9d16fe`. Source-file hashes, rather than that HEAD
alone, identify the copied working tree content.

The two fixture generators and the DD/PostgreSQL adapters retain their original
contents. The copied runner adds a native SQLite consumer path, exact-cell
selection, fixture reuse outside timing, and native binary/source fingerprints.
The original Python SQLite adapters remain preserved source references; these
commands use the Rust native consumer. Historical baseline files are hashed for
provenance and are never relabeled as measurements of this extension.

`37_fixture_export.mjs` generates `tests/fixtures/0_shared.json` for Rust/native
acceptance tests. `38_report.mjs` accepts successful complete receipts and prints
median timings. The standalone DD Cargo manifest pins differential-dataflow
0.25.1 and timely 0.31.0 with a lockfile.

## Local results

The final receipts and their generated TSV reports are under `bench/results/`.
These generated case artifacts are excluded from source archives. A concise
checked-in result record is maintained in [39_acceptance.md](39_acceptance.md).
CI executes the shared SQLite/DD/SQLite-query smoke acceptance on Linux and
macOS. Remote CI execution has not been performed from this working tree.
