# shootout-rust port, 2026-09-20

One Rust binary (`sqlite-ivm-bench`, `bench/`) replaces `bench/*.mjs|py|pl` and the
`scripts/11|16_shootout.sh` orchestration for the circuits suite. Same fixtures, same
13 states, same checksum protocol (`S\t` rows, sha256), same quick profile
(400:10:10, 1 warmup, 3 measured repetitions, median of the 13-state sum).

## Process metrics: build vs buy

The old runner observed adapter peak RSS with `/usr/bin/time` (kernel high-water
counter) and the PostgreSQL server group with 5 ms descendant RSS sampling
(`bench/shared/12_crossover_runner.mjs`). The Rust binary runs arms in-process, so
the two observations need different mechanisms.

| candidate | gives | costs | verdict |
|---|---|---|---|
| `libc::getrusage(RUSAGE_SELF)` | exact lifetime high-water RSS of the harness process; same counter `/usr/bin/time -l` prints; no sampling, no timing noise | one `libc` dependency, ~10 lines, platform unit difference (macOS bytes, Linux KiB) | buy |
| `sysinfo` | cross-platform enumeration of *other* processes; needed for the pg postmaster group sum | one dependency with broad platform surface; per-refresh cost bounded by refreshing only the snapshot pid list | buy |
| `procfs` | VmHWM + /proc parsing | Linux-only; this bench runs on darwin aarch64 first | reject |
| `libproc-rs` | `proc_pid_rusage` binding | darwin-only, resident size is current not peak | reject |
| `sysinfo` alone | process table for self + pg group | no peak metric for self; would replace an exact kernel counter with sampling | reject |

ivm RSS column = absolute harness peak RSS at case end. It includes the combined
binary baseline (DD/timely/postgres client are linked in), so it is not
directly comparable to the old per-adapter-process numbers; the ms columns carry
the 20% gate. pg RSS = sum over the postmaster process group sampled once per
state (after each `apply`) plus teardown, not the old 5 ms continuous sampler;
with per-state updates in the 3–15 ms range the two agree within the RSS
quantum, and the receipts keep the per-state samples.

Scale sweep methodology deltas vs `scale-sweep.md`: the replace column is one
sample (a 1000-row `INSERT OR REPLACE` transaction, not per-row means), the
sweep runs in one process so `rss_delta_mib` is process-lifetime delta, and
`db_bytes` is the on-disk db after `wal_checkpoint(TRUNCATE)` rather than
in-memory footprint.

Disk: SQLite db file length after `PRAGMA wal_checkpoint(TRUNCATE)`; PostgreSQL
`pg_database_size(current_database())`.

Timing: `Instant` around `apply` only (mutation transaction + output SELECT,
per state), case total = 13-state sum, median of 3 measured repetitions after 1
warmup. Verification (SQL oracle recompute, input rows, hashes) is outside the
timed windows, as before.

Shell law: the binary spawns only `initdb`, `pg_ctl`, and `gnuplot` (scale SVGs,
direct argv, no `sh -c`; charts are skipped with a stated reason when gnuplot is
absent). `cargo` is never invoked by the binary; the extension dylib is built
beforehand with `cargo build --release --features extension` at the repo root and
located next to the bench binary (or via `IVM_EXTENSION`).

## Receipts

### R1 dump-fixture parity (sha256, default dims 24/3/4/integers unless noted)

Node side: `node --input-type=module -e` calling `makeCircuitFixture` /
`makeSemanticFixture` from `bench/shared/30_circuit_workload.mjs` /
`36_semantic_catalog.mjs` with the same arguments, `JSON.stringify(x,null,2)+'\n'`,
sha256 of stdout. Rust side: `dump-fixture <circuit> [dims]`.

| circuit | node sha256 | rust sha256 | equal |
|---|---|---|---|
| pipeline | 7306c27a…bce1b2e3 | same | yes |
| fanout_fanin | 773bfce1…e52567bb | same | yes |
| distinct | 61294a5a…8a9b6ce | same | yes |
| join | b77812a7…ff1ee1040 | same | yes |
| self_join | cf125e60…af06a09 | same | yes |
| chain | 468157ba…6316c254 | same | yes |
| diamond | a6d8fdf9…06c5c9dc | same | yes |
| semijoin | 80811d69…2027cc08 | same | yes |
| antijoin | 7fda9d75…f938c61d | same | yes |
| aggregate_churn | 659aac9a…5a2f0dd | same | yes |
| reach_cycle | b8d3be21…2d9d2ab0f31 | same | yes |
| minmax | 6dd3e16b…f7a3fa48444 | same | yes |
| count_distinct | c94abc01…6ea36233 | same | yes |
| union_set | ac9d24b4…8aec537566 | same | yes |
| except_set | 3925b6f9…b9fa5d205cf | same | yes |
| intersect_set | 56e7cfc1…88e03b | same | yes |
| topk | 68884b27…20e4c3 | same | yes |
| window_rank | 0fd83cc7…b255c2b79 | same | yes |
| subquery | bd4e9571…ad91542a6 | same | yes |
| cte | f9cd999f…faba4b14 | same | yes |

All 20 default-dim fixtures are byte-identical to the node exports (compared
against the element bytes of `tests/fixtures/0_shared.json`, which is
`JSON.stringify(fixture,null,2)+'\n'`). Non-default spot checks also byte-match:
`chain 50/7/3 integers`, `join 33/5/2 text_nocase`, `self_join 29/4/6
mixed_int_real`. One fixture-builder divergence was found and fixed: the
`duplicate_support` state must copy `a[0]`'s already-encoded cells verbatim (JS
`[rows+1,...tables.a[0].slice(1)]`), not re-run them through `valueRow`.

### R2 quick shootout vs shootout-quick-2.md (median ms of 3 runs, 400:10:10)

| circuit | sqlite-ivm quick-2 | sqlite-ivm run1 | run2 | run3 | within 20% | note |
|---|---|---|---|---|---|---|
| chain | 29.0 | 29.5 | 29.3 | 27.6 | yes (+2/-5%) | |
| pipeline | 8.2 | 8.4 | 8.0 | 7.7 | yes (+2/-6%) | |
| self_join | 39.4 | 40.0 | 39.2 | 37.1 | yes (+1/-6%) | |
| reach_cycle | 54.6 | 56.1 | 56.7 | 54.6 | yes (+4/0%) | |
| aggregate_churn | 17.0 | 16.7 | 17.5 | 16.8 | yes (-2/+3%) | |

Three independent `shootout quick` invocations, each a fresh cluster, scratch
dir, and process. Every target within ±6% of `shootout-quick-2.md` (gate 20%).
Full per-engine tables land in `results/shootout-rust-<timestamp>/report.md`.

### R3 checksum parity

| engine | circuits | states | checksum mismatches |
|---|---|---|---|
| sqlite-ivm | 20 | 13 | 0 |
| pg-ivm | 20 (10 ok, 10 unsupported) | 13 | 0 |
| sqlite-query | 20 | 13 | 0 |
| pg-query | 20 | 13 | 0 |
| dd | 20 | 13 | 0 |

`shootout smoke` (24/3/4, 1 rep, 0 warmup), exit code 0, 100/100 cases: 90 ok,
10 pg-ivm `Setup::Unsupported` with SQLSTATE 0A000 and the pg error text in the
receipts (6 distinct reasons: ORDER BY, UNION/INTERSECT/EXCEPT, DISTINCT
aggregate, recursive query, window functions, general restriction). Every ok
cell equals the fixture oracle checksum for all 13 states.

### R4 clippy

`cargo clippy -p sqlite-ivm-bench -- -D warnings`: 0 errors, 0 warnings.

### R5 root suite unchanged

`cargo test` at repo root: all suites ok — 75 passed, 1 ignored (pre-existing),
0 failed, matching origin/main (the count includes the restored
`6_extension_load` rail after merging main; the arc itself touches no
`tests/**` paths).
