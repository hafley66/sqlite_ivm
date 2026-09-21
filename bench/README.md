# sqlite-ivm-bench

The Rust harness runs circuit shootouts, scale sweeps, and fixture dumps.
`crossover/` preserves the earlier sprefa fixture generator and DD consumer,
with an adapter for the current native plugin.

```bash
just shootout smoke
just shootout quick
just crossover
just scale
```

Run these recipes from the repository root. Each builds the required artifacts.
Native extension builds use `target/extension` to avoid overwriting the linked
Rust library used by the benchmark binary.

## Subcommands

### `bench shootout [smoke|quick] [--engines e1,e2] [--out DIR] [--circuits c1,c2] [--pg-prefix DIR]`

Runs every circuit in `bench/shared`-parity fixture space against up to five
engines: `sqlite-ivm`, `pg-ivm`, `sqlite-query`, `pg-query`, `dd`.

- Profiles: `smoke` = 24 rows / 3 batch / 4 fanout, 1 rep, 0 warmups;
  `quick` = 400 / 10 / 10, 3 reps, 1 warmup pass per case.
- Engines run in configured order; each rep rebuilds the arm from scratch.
- Timing wraps `apply` only (mutation + materialize); verification and
  lifecycle checks are untimed. Case number = median over timed reps.
- pg cases share one temp cluster per invocation (`initdb --auth=trust
  --no-locale --encoding=UTF8`, `pg_ctl -m immediate stop` on drop). Views
  pg_ivm refuses (SQLSTATE 0A000) print `n/a (<reason>)` and do not fail the
  run.
- Outputs: `circuits.jsonl` (per-rep receipts), `report.md` (quick-2 columns),
  `report.json`, and the same table on stdout.
- Exit codes: `0` all checksums matched, `1` fixture/oracle mismatch,
  `2` execution failure.

Requires the extension dylib for `sqlite-ivm`: build it at the repo root with
`bash scripts/0_build.sh release` (the harness resolves
`libsqlite_ivm.{dylib,so}` next to its own binary, or set `IVM_EXTENSION`).
For pg engines pass `--pg-prefix` or `IVM_POSTGRES_PREFIX`.

### `bench scale [--circuits ...] [--n 10,100,...] [--fanout 1,10] [--arms a1,a2] [--reps N] [--out DIR]`

Arm sweep. Circuits: `chain`, `join`, `group`, `distinct`, `window`, `reach`;
seed `k=(id*7)%(n/fanout+1)`, `v=(id*13)%(n/fanout+1)` into a/b/c in one
transaction; WAL/NORMAL pragma set. Arms: `sqlite-ivm` (in-process
`sqlite_ivm::extension::register` plus a virtual table), `sqlite-query` (the
circuit query run directly), `dd` (the scale dataflow graphs in
`scale_dd.rs`). Every arm takes the same write stream, and each cell's final
read is checked against a plain in-memory recompute. `--reps N` runs every
cell N times, one `rep` column per row; the SVGs plot the median rep.

- Columns: `wall_ms` is the whole cell, writes plus reads. insert/delete/update
  are means over 40 single-statement writes; `replace_ms` is a single 1000-row
  `INSERT OR REPLACE` transaction (one sample — 40 repeats of a 1000-row
  transaction would blow the bounded-run budget at n=100000); `recompute_ms`
  is the mean of 20 reads, 3 at n>=10000, 1 at n>=100000; `peak_rss_mib` is the
  getrusage maxrss growth across the cell (process high-water, so early cells
  absorb startup); `disk_read_bytes`/`disk_write_bytes` are the
  `proc_pid_rusage` (`/proc/self/io` on Linux) counters over the cell;
  `db_bytes` after `PRAGMA wal_checkpoint(TRUNCATE)`; `arrangement_rows`
  counts `__ivm_v_*` tables in `sqlite_schema`.
- `ivm_over_dd` is `wall_ms(sqlite-ivm) / wall_ms(dd)` on the sqlite-ivm row.
- Writes `scale.tsv` incrementally, renders one wall SVG and one `ivm/dd` SVG
  per circuit via gnuplot (log axes when the data spans >10x), and prints a
  `defect:` line for every column over 10 s.

### `bench dump-fixture <circuit|all> [--rows N] [--batch N] [--fanout N] [--domain D]`

Prints the fixture (same JSON shape as `bench/37_fixture_export.mjs`,
including key order) to stdout. Domains: `integers` (default), `text_nocase`,
`mixed_int_real`. Used for the sha256 parity receipt against the node
generators.

## Receipts

See `plans/costs/shootout-rust.md` for the receipt slots (R1-R5): fixture
parity, checksum match, quick-2 comparison, clippy, and the root test suite.

## Recovered crossover

`just crossover` runs all nine historical rows/batch/fanout cases with three
repetitions and no warmups. Arm order alternates each repetition. SQLite uses
WAL/FULL; DD remains volatile. Every state is checked against the original
input and output hashes. Results, process logs, database files, source/binary
hashes, and invocation time are retained in `bench/results/crossover-*`.
See [crossover/0_SOURCE.md](crossover/0_SOURCE.md) for provenance.

## Historical comparison and observation

```bash
cd /Users/chrishafley/projects/sqlite_ivm && just crossover-observe
```

Builds the historical counted C extension from local `sprefa@e2052d5ae`, builds
the current plugin and DD consumer, and runs nine cases with three repetitions.
Arm order rotates. `--sprefa PATH` selects another local sprefa repository.
The historical source and its hashes are retained beside the results.

After timing completes, a linked Rust host replays the 12,000/1,000/200 fixture
under hafley-observe for both SQLite arms. Each mutation produces a Chrome trace,
a queryable SQLite event database, and entries in `profile.json`: SQL calls,
profile durations, query plans or their errors, operator/site attribution,
total and p99 statement durations, CPU, peak RSS, and disk writes.
Fixture and host binary hashes identify the capture. Both independent source
arrays and aggregate results are checked against the fixture and ordinary SQL.
The SQL workloads and engine-specific interpretation stay in this repository;
capture, event aggregation, timeline export, event storage, and process sampling
come from hafley-observe.

Diagnostic wall times include capture and sink overhead. Nested SQLite profile
times overlap; raw VM counters accumulate on reused statement handles. Neither
sum represents total work. Historical C has SQL events but no Rust operator spans.
Both timing arms use WAL/FULL, but the historical source-view arm has an 8 MiB
page cache and a source-key index; the current arm uses default cache settings
and persistent arrangements. Historical population is incremental; current
population precedes view creation. Setup is outside the mutation timer.
