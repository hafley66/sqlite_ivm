# Brief: one Rust bench binary replaces `bench/` and the shootout scripts

Two arcs, one PR each. Arc A lands before arc B starts.

## Goal

`cargo run --release -p sqlite-ivm-bench -- shootout quick --engines sqlite-ivm,sqlite-query,pg-ivm,pg-query,dd --out DIR`
reproduces the numbers in `plans/costs/shootout-quick-2.md` (same fixtures,
same 13 states, same checksums) from one Rust binary. Then every mjs, sh, py,
pl file under `bench/` and every shootout wrapper under `scripts/` is deleted.

## Repo

`~/projects/sqlite_ivm`. Worktree from `origin/main`. Branch `refactor/bench-rust`.

## Owned files

Arc A creates:
- `bench/Cargo.toml` (rewrite: package `sqlite-ivm-bench`, own `[workspace]`, one `[[bin]] bench`)
- `bench/src/main.rs`, `bench/src/fixture.rs`, `bench/src/oracle.rs`,
  `bench/src/arms/{sqlite_ivm,sqlite_query,pg_ivm,pg_query,dd}.rs`,
  `bench/src/report.rs`
- `bench/README.md` (rewrite)

Arc B deletes: every file under `bench/` and `bench/shared/` that is not in the
list above, plus `scripts/11_shootout.sh`, `scripts/16_shootout.sh`,
`scripts/7_native_shared.mjs`, `scripts/8_native_shared.sh`,
`scripts/12_features.sh`, `scripts/13_feature_pg.sh`, `scripts/14_native_values.sh`,
`scripts/15_pg_baseline.sh`, `examples/4_sqlite_case.rs`, `examples/5_feature_case.rs`, `tests/14_scale.rs`
and their `[[example]]` blocks and the `bench` feature in the root `Cargo.toml`.

Forbidden: `src/**`, `tests/**`, `docs/**`, `plans/**` except
`plans/costs/shootout-rust.md` (new, arc A receipt).

## Source of truth to port (read before writing)

| what | from |
|---|---|
| tables, 11 circuits, 13 states, oracle | `bench/shared/30_circuit_workload.mjs` |
| 9 semantic circuits and oracle | `bench/shared/36_semantic_catalog.mjs` |
| value domains | `30_circuit_workload.mjs:19-21` |
| sqlite arm (extension load, pragmas, per-state loop, checksum) | `examples/4_sqlite_case.rs` |
| pg arms (cluster start, `CREATE EXTENSION pg_ivm`, `create_immv`) | `bench/shared/31a_circuit_postgres.mjs`, `32_circuit_postgres.mjs`, `bench/43_pg_full_using.sql` |
| DD arm | `bench/shared/34_circuit_dd.rs`, `38a_semantic_dd.rs` |
| output/checksum format | `30_circuit_workload.mjs:26-27` (`S\t` rows, sha256) |
| report columns | `bench/51_shootout_report.mjs`, `plans/costs/shootout-quick-2.md` |
| RSS and disk measurement | `bench/53_shootout.mjs` (search `rss`, `disk`) |

Drop with no port: prolog arm, pglite arms, feature fixtures
(`tests/fixtures/1_features.json` stays for `tests/4_features.rs`, the bench does not read it).

## Second subcommand: `scale`

Replaces `tests/14_scale.rs` (delete it in arc B, with its `plans/costs/scale-sweep.md` numbers kept as history).

`bench scale --circuits chain,group,distinct,topk,reach --n 10,100,1000,10000,100000 --fanout 1,10 --out DIR`

- Disk db, `journal_mode=WAL`, `synchronous=NORMAL`, never in-memory.
- Seed shape from `tests/14_scale.rs:31-53` with `fanout` as a parameter:
  fanout 1 is one match per join key (dictionary shape), fanout 10 is the
  current shape. Both reported.
- Write columns: single insert, single delete, single update, 1000-row
  replace in one transaction. Each mean of 40, recompute capped at 3 reps
  above 10000.
- Cost columns per cell: write ms, recompute ms, peak RSS delta MiB (process
  metrics crate from the build-vs-buy table), db bytes after `wal_checkpoint(TRUNCATE)`,
  arrangement row count (`SELECT count(*)` over every `__ivm` table for the view).
- Output: TSV to `DIR/scale.tsv` plus one SVG per circuit via gnuplot, the
  `bench/55_shootout_plot.mjs` pattern ported.
- Whole sweep under 10 minutes in the background; any cell over 10 s is
  printed as a defect line, not hidden.

## Design laws

- Fixture is one `struct Fixture { circuit, query, columns, rows, states: Vec<State> }`,
  `State { name, mutation_sql, expected_checksum }`. Generated in Rust, never
  read from JSON. `--dump-fixture circuit` prints it as JSON for diffing.
- Arm is one trait: `fn setup(&mut self, f: &Fixture) -> Result<()>`,
  `fn apply(&mut self, s: &State) -> Result<Measure>`, `fn teardown`.
  `Measure { wall: Duration, checksum: String }`.
- RSS from `/proc/self` or `libproc` via the `sysinfo` crate; disk from the db
  file size after checkpoint. Build-vs-buy table for the process-metrics crate
  goes in `plans/costs/shootout-rust.md` before the first commit that uses it.
- Timing is `Instant` around `apply` only. Warmup 1, repetitions 3, median.
- Every loop bounded, `for state in states` only; no `loop {}`.
- Postgres: `IVM_POSTGRES_PREFIX` env, default `bench/shared/.work/postgres-18.6`
  (stays, gitignored). Cluster in a temp dir, `pg_ctl -m immediate stop` on exit.
- pg_ivm rejects `reach_cycle`; the arm returns `Unsupported` and the report
  prints `-`, never an error.
- Zero shell: no `Command` except `pg_ctl`, `initdb`, `cargo` is not called.
- `eprintln!` never; `tracing` only.
- Comments: constraints only. No change-log narrative.

## Receipts

Arc A:
1. `cargo run --release -p sqlite-ivm-bench -- dump-fixture chain --rows 400` sha256
   equals `node bench/shared/37_fixture_export.mjs` output for the same args
   (run both, paste both hashes in `plans/costs/shootout-rust.md`).
   Do this for all 20 circuits in a loop; table of 20 rows, all equal.
2. Quick shootout, three runs, table of median ms per circuit per engine beside
   the `shootout-quick-2.md` numbers. Every sqlite-ivm number within 20% or a
   named reason.
3. Every arm reports the same checksum as the fixture on every state.
4. `cargo clippy -p sqlite-ivm-bench -- -D warnings` clean.
5. Root `cargo test` unchanged (72 passing legs, none new).

Arc B:
1. `git diff --stat origin/main...HEAD` shows only deletions and the four
   config edits.
2. Quick shootout runs from a clean checkout with only `cargo` and the
   postgres prefix.
3. `grep -rn "bench/shared\|53_shootout\|4_sqlite_case" --include=*.md --include=*.rs --include=*.toml --include=*.sh .`
   returns nothing outside `chat_log/`, `plans/`, `labs/`, `probes/`.

## Report back

`boop beep --no-wait --as refactor/bench-rust sprefa-coordinator "<one line>"` at
each PR. Line carries PR number, receipt numbers met, receipt numbers missed
with exact error text.

## Ten-second law

No single test or command over 10s in the foreground. The shootout runs in the
background with output to a file.
