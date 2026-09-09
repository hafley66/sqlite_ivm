# One-command engine coverage and timing

From the repository root:

```bash
just ivm-shootout
# Equivalent without Just:
bash sqlite_ivm/scripts/16_shootout.sh
```

The command builds its Rust binaries, discovers installed SQLite and task-local
PostgreSQL, installs locked Node dependencies if absent, starts an isolated
PostgreSQL cluster on a Unix socket, executes the checks below, measures the
selected workloads, stops PostgreSQL, and prints the report. No external running
database is used. Every invocation creates a fresh result directory.

## Engines and actual execution

| Engine | Execution |
|---|---|
| sqlite-ivm | Native loadable Rust extension, persistent SQLite operator state |
| dd | Native Differential Dataflow 0.25.1 / Timely 0.31.0 graphs |
| prolog | SWI-Prolog predicates; incremental tabling for the recursive circuit |
| pg-ivm | Installed native PostgreSQL with pg_ivm |
| pg-query | Ordinary PostgreSQL queries after source mutations |
| sqlite-query | Ordinary SQLite queries after source mutations |
| pglite-ivm | PGlite with its bundled pg_ivm extension |
| pglite-query | Ordinary PGlite queries after source mutations |

Every engine is requested by default. Missing installations are printed as
unavailable and make the command exit 2. Set IVM_POSTGRES_PREFIX to a prefix
containing bin/postgres and pg_ivm; an existing task-local installation is
also discovered and its exact prefix recorded. For a fresh PostgreSQL build:

```bash
bash sqlite_ivm/bench/shared/1_prepare_native.sh
```

SWI-Prolog must provide swipl on PATH. SQLite, Cargo and Node must be installed.
On macOS, the command selects Homebrew SQLite when SQLITE3_LIB_DIR is unset.
For intentionally narrower execution, name the requested engines explicitly:

```bash
just ivm-shootout smoke --engines sqlite-ivm,dd,prolog,sqlite-query
```

## Semantic checks before performance

- 20 shared circuit families, with exact source and output bags checked after
  each of 13 mutations in every admitted engine. The circuit set includes
  projection/filter, fan-in, duplicates, joins, self/chain/diamond joins,
  semi/antijoin, aggregate churn, cyclic reachability, min/max, distinct
  aggregates, sets, top-k, rank, subqueries and CTEs.
- 44 typed SQL compositions, with 174 states each. The independent Prolog
  implementation computes the fixture's answers through predicates, without
  executing SQL. DD applies signed keyed changes. SQLite/PostgreSQL/PGlite
  exercise their SQL statements, including transaction and savepoint changes.
  Prolog and DD consume keyed differences between the observed input states;
  they do not claim to execute SQL transaction syntax.
- 13 DD contracts outside the SQL-facing catalog: signed map/filter/flatmap;
  signed concatenation and negation; weighted joins; shared arrangements with
  multiple consumers; custom weighted reduction; count; positive threshold;
  binary transitive closure; mutually recursive even/odd reachability;
  recursive minimum distance; nested fixed points; stratified antijoin after
  recursion; and incomparable timestamps with join least-upper-bound,
  retraction and held-frontier checks. Independent maps and graph traversal
  compute the expected results. These are a finite executable contract catalog,
  not a proof for arbitrary user programs or every DD API.
- Native extension value and transaction tests cover the additional SQLite
  storage, rollback and concurrency contract.

The report distinguishes PASS, UNSUPPORTED (an executed engine rejected the
query), UNWIRED (this suite has no equivalent adapter), ORACLE, MISMATCH,
UNAVAILABLE, and INCOMPLETE. A DD-specific contract without another engine
adapter does not imply that the other engine cannot express that computation.

The Prolog feature predicates recompute their result bags. The shared recursive
Prolog circuit uses incremental tabling. SQLite/PG use durable database state;
DD and Prolog are volatile. PGlite uses NodeFS with WASM host-dependent fsync
behavior. The report retains these execution differences beside timings.

## Performance workloads

| Profile | Rows | Warmups | Measured repetitions | Circuits |
|---|---|---|---|---|
| smoke | 24 | 0 | 1 | All 20; execution check |
| quick, default | 400 | 1 | 3 | All 20 |
| full | 400 and 12,000 | 1 | 5 | Pipeline, join, aggregate churn, reachability, min/max, distinct count |

```bash
just ivm-shootout full --details
just ivm-shootout quick --out /tmp/ivm-comparison-run
```

Quick charts contain the measured 400-row tier for all 20 workload families.
Full charts contain the measured 400-row and 12,000-row tiers for its six
selected workload families. Chart lines connect measured tiers only; they do
not extrapolate between or beyond those inputs.

The dimensions are printed as rows, batch size and fanout. Each case applies
initial loading, duplicate support, support removal, a batch key/value move,
right/third-side changes, root/edge clearing, a cyclic seed, alternate-path
removal, root removal/restoration and a cycle break. These are generated
controlled workloads; performance on unrelated application data needs its own
fixture and oracle.

Timed work includes mutations, maintenance completion and output materialization.
Source/output validation and fixture generation occur outside that interval.
After each validated state, an untimed `state_inventory` snapshot records physical
relations, roles, row counts, allocated/data/index bytes, database and WAL sizes,
and metric-specific unavailable reasons. PostgreSQL's database size is the
overhead-inclusive result of `pg_database_size`, separate from relation sizes.
SQLite relation allocation comes from `dbstat`, separate from filesystem length.
DD and Prolog durable-storage metrics are unavailable rather than zero; process
peak RSS is recorded by the parent runner with its process or process-group scope.
The process receipt also retains the raw `/usr/bin/time` output and normalizes
available CPU time, elapsed time, faults, swaps, context switches, instructions,
cycles and block-I/O operations. PostgreSQL adapters record `pg_stat_database`
deltas for cache blocks, tuples, transactions and temporary files/bytes. Those
are database-wide page-cache and logical activity counters since the case
baseline, including validation and telemetry queries; they are not physical
disk operation or byte counts.
The resulting run is instrumented: catalog/count/dbstat probes can warm caches
before the next mutation. Process high-water RSS covers the adapter, validation,
and telemetry code. PostgreSQL process-group RSS can include shared pages in more
than one process. It is not a measurement of the maintained view's heap alone.
The report separates initial load, whole-edge clearing and the sum of the other
11 mutation states, taking medians across measured repetitions. Warmups are
excluded. Missing states, repetitions, failed semantic gates or wrong input/output
hashes exclude a timing. Engine order rotates deterministically between rounds.

A child process has a 120-second limit and each circuit phase has a 20-minute
limit. Timeouts remain failures. There is no enforced total memory cap or CPU
isolation, so these runs do not establish resource-isolated performance.

## Reports, exit codes and retained data

The command prints semantic coverage, each DD contract, the shared circuit
matrix, mismatches, and timing rows. Add --details for the typed-query matrix.
ANSI colors are enabled on terminals; --color forces them and NO_COLOR disables
automatic color. Plain text remains valid when redirected.

Each result directory contains:

- report.txt: the terminal report without escape codes.
- report.json: structured coverage, statuses, timings, source fingerprints and phase outcomes.
- coverage.tsv: every requested engine/case, including unsupported and unwired cases with reasons.
- run.json: commands, revision, source hashes, dependencies, logs and exit statuses.
- circuits.jsonl, performance.jsonl, dd-contracts.jsonl and features/*.jsonl: execution receipts.
- charts/index.html and charts/index.md: clickable indexes for the overview and one numbered PNG per measured workload family.
- charts/overview.png and charts/NN_workload.png: dark-background coverage and per-workload timing/state charts. PNG generation requires gnuplot; the data exports are still written when it is unavailable.
- charts/telemetry.json and charts/telemetry.tsv: raw receipt metrics, report telemetry, per-state and per-phase latency distributions, throughput, process resources and unavailable reasons.
- charts/inventory.json and charts/inventory.tsv: every state inventory snapshot and relation-level count, role, allocation, data/index byte metric and unavailable reason.
- charts/manifest.json: source paths, generated files, workload cells and PNG availability.
- Shared fixtures and per-process stdout/stderr for reproduction.

Successful per-case database copies are discarded after their receipts are
validated. Set IVM_KEEP_DATABASES=1 to retain them. Failing cases retain their
available artifacts. Previously generated experiment data is unaffected.

Exit 0 means the requested adapters completed without observed mismatches or
missing execution; unsupported query definitions remain explicit. Exit 1 means
an observed result mismatch. Exit 2 means missing dependencies, incomplete
receipts, timeouts, build failures, or other execution errors. The command still
prints its completed report when a competitor fails.

Native pg_ivm 1.15 and PGlite's pg_ivm 1.13 both reproduce the FULL JOIN USING
merged-key deletion mismatch in this fixture. The complete default command
therefore currently exits 1. The separate CI baseline checks require those exact
observed rows; they do not turn that mismatch into a semantic pass.

CI runs the unified command with SQLite/DD/Prolog on Linux and macOS, tests the
report's failure detection, and checks native PostgreSQL/PGlite feature receipts
in its PostgreSQL job.

## Recorded command execution

The 2026-09-09 local quick run completed all phases. Its
[report](receipts/1_unified_20260909/report.txt),
[structured results](receipts/1_unified_20260909/report.json),
[coverage matrix](receipts/1_unified_20260909/coverage.tsv), and raw receipts are
committed under receipts/1_unified_20260909. It records 140 validated performance
rows with three measured repetitions per row. Native pg_ivm 1.15 and bundled
pg_ivm 1.13 each have the recorded using_full mismatch; there are no incomplete
or other failed execution cells in that run. The report therefore returns the
mismatch status and exit code 1.

The recorded instrumented quick run is described by its
[receipt README](receipts/3_telemetry_20260909/README.md) and
[chart index](receipts/3_telemetry_20260909/charts/index.md). It covers all 20
workloads at the 400-row tier, with 140 validated timing cells, 20 unsupported
cells, 5,460 measured state inventories, and 480 process-resource samples.
