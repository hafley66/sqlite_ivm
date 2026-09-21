# sqlite-ivm-bench

One Rust harness replacing the `bench/*.mjs`, `scripts/8-16*`, and
`examples/4|5` shell-script era: circuit shootout, scale sweep, and fixture
dumps. Zero shell — the only external programs are `initdb`, `pg_ctl`, and
`gnuplot`. No `eprintln!`; logging goes through `tracing`.

## Subcommands

### `bench shootout [smoke|quick] [--engines e1,e2] [--out DIR] [--circuits c1,c2] [--pg-prefix DIR]`

Runs every circuit in the shared-fixture-parity space against up to five
engines: `sqlite-ivm`, `pg-ivm`, `sqlite-query`, `pg-query`, `dd`.

- Profiles: `smoke` = 24 rows / 3 batch / 4 fanout, 1 rep, 0 warmups;
  `quick` = 400 / 10 / 10, 3 reps, 1 warmup pass per case.
- Engine order rotates per rep; each rep rebuilds the arm from scratch.
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
`cargo build --release --features extension` (the harness resolves
`libsqlite_ivm.{dylib,so}` next to its own binary, or set `IVM_EXTENSION`).
For pg engines pass `--pg-prefix` or `IVM_POSTGRES_PREFIX`.

### `bench scale [--circuits ...] [--n 10,100,...] [--fanout 1,10] [--out DIR]`

Port of the script-era scale driver (git history). Circuits: `chain`, `group`,
`distinct`, `topk`, `reach`; seed `k=(id*7)%(n/fanout+1)`,
`v=(id*13)%(n/fanout+1)` into a/b/c in one transaction; WAL/NORMAL pragma set;
in-process `sqlite_ivm::extension::register`.

- Columns: insert/delete/update are means over 40 single-statement writes;
  `replace_ms` is a single 1000-row `INSERT OR REPLACE` transaction (one
  sample — 40 repeats of a 1000-row transaction would blow the bounded-run
  budget at n=100000); `recompute_ms` is the mean of 20 reads, 3 at n>=10000,
  1 at n>=100000; `rss_delta_mib` is the getrusage maxrss delta across the
  cell; `db_bytes` after `PRAGMA wal_checkpoint(TRUNCATE)`;
  `arrangement_rows` counts `__ivm_v_*` tables in `sqlite_schema`.
- Writes `scale.tsv`, prints it, renders one SVG per circuit via gnuplot
  (log axes when the data spans >10x), and prints a `defect:` line for every
  cell over 10 s.

### `bench dump-fixture <circuit|all> [--rows N] [--batch N] [--fanout N] [--domain D]`

Prints the fixture (same JSON shape as the script-era node exporter,
including key order) to stdout. Domains: `integers` (default), `text_nocase`,
`mixed_int_real`. Used for the sha256 parity receipt against the node
generators.

## Receipts

See `plans/costs/shootout-rust.md` for the receipt slots (R1-R5): fixture
parity, checksum match, quick-2 comparison, clippy, and the root test suite.
