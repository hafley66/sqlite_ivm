# watch the watchman

Receipts for the `hafley-observe` pricing lab, 2026-09-21. Base `origin/main`
at `3660640`. Lane worktree
`.boop-worktrees/feature/watch-the-watchman`.

One command prints one table:

```
just watch-the-watchman
```

Every cell is a difference between one binary with a candidate layer on and
the same binary with it off. A single absolute number is not a receipt, and
none appears here as one.

## What the lab is

The lab is the crate. There is no `labs/` directory, no copy of
`hafley-observe`, and no vendored fork. Each candidate layer is a cargo
feature of the crate, each sink is a public item, and the harness is a binary
target of the crate that consumes the same public surface a host does.
A finding that cannot be expressed as a feature of this crate is not a
finding.

## The workload

One workload, shared by every row. It lives in
`crates/hafley-observe/bench/watch_the_watchman.rs` with its parameters in one
block at the top of the file:

| constant | meaning |
|---|---|
| `PARAGRAPHS` | outer spans |
| `LINES_PER_PARAGRAPH` | middle spans under each outer span |
| `TOKENS_PER_LINE` | hot inner spans under each middle span |
| `GLYPHS_PER_TOKEN` | a field every innermost event repeats |
| `LINE_WIDTH` | a second repeated field |

Shape: an `info` span per paragraph, a `debug` span per line, a `trace` span
per token, and one `trace` event inside each token span. Nested spans, a hot
inner span, and a field name set that repeats on every event.

The workload is deterministic. It draws no random number, reads no clock to
decide anything, and takes no input. Two runs of the same binary emit the same
events in the same order. A reader asking about seeds gets this paragraph.

The timed region is the workload and the closing flush. Subscriber
construction, the sink's schema, and the OTLP provider setup sit outside it,
because they are paid once and the table prices the per-event path.

## Method: the differential

For every candidate:

1. Build the harness with the candidate off, and with it on. Same profile,
   same `CARGO_TARGET_DIR`, release.
2. Run the workload three times on each side, once per flush strategy.
3. Report the percent cost as the median on over the median off, and print all
   six raw wall numbers beside it.
4. The cell reads `cost` only when all three on-runs sit outside the spread of
   all three off-runs. Otherwise it reads `in the noise` and prints the six
   numbers that say so.

The off side is a binary with no layers at all, not a filter set to `error`.
The harness prints the layer list it was compiled with, and the driver refuses
an off row whose list is not `none`. See failure mode 17.

The bench gets its own target directory,
`$CARGO_TARGET_DIR/watch-the-watchman`, so its release builds do not contend
with the lane's debug builds. Every measurement log, database and timeline file
lives under that directory, never under `/tmp`.

## Decisions, and the constraints that forced them

| decision | constraint |
|---|---|
| The four shipped layers (`fmt`, `chrome`, `otlp-trace`, `sqlite-sink`) stay in `default` | `boop`, `soopy` and `sprefa-extract` build this crate with default features and are out of scope. Turning a default off would silently remove a layer from a host that nobody in this lane may edit. Every candidate this lab adds is off by default and independently enableable; the off side of a measurement is `--no-default-features`. |
| The flush strategy is read by `Config::flush()` from `HAFLEY_FLUSH`, not stored in a `Config` field | `crates/boop/tests/native_projector_contention.rs` builds `hafley_observe::Config` as a struct literal with five fields, and `crates/sprefa-extract/src/trace.rs` does the same for `FormatConfig`. A sixth field breaks both crates. The strategy is still one enum on the crate's public configuration surface, obeyed by every sink; only its storage moved. |
| Every candidate feature keeps its public constructor when off | The same three host crates call `format_layer`, `chrome_layer` and the sqlite module. With a feature off the constructor returns an identity or `None`, so a host compiles unchanged and the binary carries nothing. |
| The rusage sampler is compiled into every build; the `rusage` feature adds only the publishing layer | The harness needs a sampler on the off side too, or it cannot report peak RSS or disk bytes for the differential. The `rusage` row therefore prices the layer, and says so in its notes. |
| `fmt` writes to a null writer | The harness's stdout is its product. Formatting is priced, terminal I/O is not. |
| Both Tracy features take one client, so `tracy-alloc` uses `tracy-client` and not `tracy_full` | `tracing-tracy` and `tracy_full` each vendor a `tracy-client-sys`; two of those in one binary is a duplicate-symbol link and then a crash, and the brief requires `--all-features` to build. `tracy-client` is the client `tracing-tracy` already uses, and it ships a tracked allocator of its own, so one client serves the layer and the allocator. `tracy_full`'s layer collects no callstack, which is the thing the `tracy` row is meant to price, so the layer stays on `tracing-tracy`. |
| OTLP rows point at `127.0.0.1:4318` with nothing listening | No collector is available to the lane, and a faked receiver is a bespoke telemetry format. The row prices the layer plus a failing exporter, and says so in its notes. |

## Candidates

| feature | crate | what the layer does |
|---|---|---|
| `fmt` | `tracing-subscriber` fmt | text or JSON log line per event |
| `chrome` | `tracing-chrome` | timeline file, written from the layer's own thread |
| `otlp-trace` | `opentelemetry-otlp` trace | batch span exporter |
| `otlp-metrics` | `opentelemetry_sdk` and `-otlp` metrics | meter provider, periodic reader, span counter and duration histogram |
| `sysmetrics` | `opentelemetry-system-metrics` | bought process observer on its own runtime, OTel meter |
| `procmetrics` | `metrics-process` | bought process collector through the `metrics` facade, sampled on a bounded span cadence |
| `metrics-ctx` | `metrics-tracing-context` | bought span-field label layer over the facade recorder |
| `tracy` | `tracing-tracy` | sampled callstacks |
| `tracy-alloc` | `tracy-client` | tracked global allocator, installed by the binary |
| `rusage` | the lifted `proc_pid_rusage` code | publishes a usage record per span close |
| `sqlite-sink` | the relational sink | log records as rows, dictionary-encoded |

The version pinned for `opentelemetry-system-metrics` is `0.32`, the release
that pairs with the otel `0.32` this crate already carries. The brief's `0.4`
line predates that pairing.

Every candidate on the brief's list was made to work and appears in the table.
No candidate was dropped, so no row cites a throw site. The two additions this
lab would propose, and did not add, are a `logs` exporter for the OTLP signal
that is still missing and a bounded-ring sink that keeps the newest rows rather
than the oldest; both are features of this crate and neither is in the brief's
list.

## The relational sink

The rule, and the reason for the lab: log records are relational data, and
writing repeated strings into rows is denormalization. The dictionary sink
stores each repeating column once, with a surrogate `INTEGER` key, and the
event row carries the key:

```sql
CREATE TABLE log_span  (id INTEGER PRIMARY KEY, name   TEXT NOT NULL, UNIQUE(name));
CREATE TABLE log_target(id INTEGER PRIMARY KEY, target TEXT NOT NULL, UNIQUE(target));
CREATE TABLE log_file  (id INTEGER PRIMARY KEY, file   TEXT NOT NULL, UNIQUE(file));
CREATE TABLE log_level (id INTEGER PRIMARY KEY, level  TEXT NOT NULL, UNIQUE(level));
CREATE TABLE log_field (id INTEGER PRIMARY KEY, field  TEXT NOT NULL, UNIQUE(field));
CREATE TABLE log_event (id INTEGER PRIMARY KEY, ts_ns INTEGER NOT NULL,
  span_id INTEGER NOT NULL REFERENCES log_span(id),
  target_id INTEGER NOT NULL REFERENCES log_target(id),
  file_id INTEGER NOT NULL REFERENCES log_file(id),
  line INTEGER NOT NULL,
  level_id INTEGER NOT NULL REFERENCES log_level(id));
CREATE TABLE log_value (event_id INTEGER NOT NULL REFERENCES log_event(id),
  field_id INTEGER NOT NULL REFERENCES log_field(id), value TEXT NOT NULL,
  PRIMARY KEY(event_id, field_id)) WITHOUT ROWID;
```

No composite TEXT primary key appears anywhere. The all-TEXT control sink has
the same two tables with every key inlined, so the only difference between the
two measured sinks is where the repeated strings live.

The measured rule from the sibling repo is applied: intern a column whose key
repeats across many rows, store a column that is one identity per row as it
is. Field names, span names, targets, file paths and levels repeat and are
interned. Field values and timestamps do not repeat and are stored per row.

Interning happens at the boundary and is cached: a hit costs a hash lookup, a
miss costs one `INSERT OR IGNORE` plus one `SELECT`. `UNIQUE` is the dedup, per
the relational design law. The cache is bounded, and clearing it on overflow is
correct because the side tables are append-only.

## What is bounded

Every `loop` in `src` names the constant that bounds it, in a comment directly
above it, and `tests/bounded_loops.rs` scans the sources, lists every loop
against its budget line, and fails if a loop has none or names a constant the
same file does not declare. The bound constants:

| constant | value | what it protects |
|---|---|---|
| `DRAIN_ROW_BOUND` | `4096` | the memory ceiling on one drain sink |
| `DRAIN_BATCH_BOUND` | `512` | how long one drain pass may hold a sink |
| `DRAIN_WAIT` | `250` ms | how long a drain waits before writing what it holds |
| `COMMIT_ROW_BOUND` | `4096` | the ceiling on an on-commit buffer that never commits |
| `FLUSH_WAIT_STEPS` | `5000` | how long a flush waits for in-flight rows |
| `DICTIONARY_CACHE_BOUND` | `8192` | the memory ceiling on interning |
| `INSTRUMENT_CARDINALITY_BOUND` | `512` | the instrument table the facade bridge may grow |
| `OBSERVER_SAMPLES` | `2` | passes the system observer takes before it ends |

When the drain bound is hit the drain stops and reports the diagnostic
`observe.drain.bound` once, and the emitting thread writes that row itself, so
the bound never loses a row. There is no recursion in this crate; the one
`loop` is the drain, and it is bounded by the two constants above.

## Flush strategies

`Flush` is one enum on the crate's public surface, and every sink obeys it.

| strategy | when the sink is written |
|---|---|
| `immediate` | on every row, on the emitting thread |
| `drain` | buffered, written by a background drain at `DRAIN_WAIT` or `DRAIN_BATCH_BOUND`, whichever comes first |
| `on-commit` | buffered, written when the host declares a commit point |

`tests/flush_contract.rs` holds all three to the same contract: every row
lands, once, in order. It also pins the difference between the strategies, so
a strategy that stops gating fails.

## Receipts

All twelve builds pass. The features that pull a Tracy client are the ones that
had to change for this to be true; see the decision on one client below.

```
$ cargo check -p hafley-observe --all-targets --no-default-features --features <feature>
fmt            ok
chrome         ok
otlp-trace     ok
otlp-metrics   ok
sysmetrics     ok
procmetrics    ok
metrics-ctx    ok
tracy          ok
tracy-alloc    ok
rusage         ok
sqlite-sink    ok
$ cargo check -p hafley-observe --all-targets --all-features
ok
```

The link is the part `cargo check` cannot see, so the tests are the proof.
`cargo test -p hafley-observe --all-features`, three runs, `exit 0` each:

```
run 1 exit=0
run 2 exit=0
run 3 exit=0
```

The stdout of run 1, verbatim. Run 2 and run 3 differ only in the durations
the harness prints:

```
warning: unused doc comment
  --> crates/hafley-observe/bench/watch_the_watchman.rs:20:1
   |
20 | / /// The tracked allocator is a property of the binary, so the harness ...
21 | | /// it. With the feature off the line does not exist and the default a...
22 | | /// stands.
   | |_----------^
   |   |
   |   rustdoc does not generate documentation for macro invocations
   |
   = help: to document an item produced by a macro, the macro must produce the documentation as part of its expansion
   = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: `hafley-observe` (bin "watch-the-watchman") generated 1 warning
warning: `hafley-observe` (bin "watch-the-watchman" test) generated 1 warning (1 duplicate)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.28s
     Running unittests src/lib.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/hafley_observe-07a6758592cb5eaa)

running 1 test
test _0_types::tests::output_format_vocabulary_is_fixed ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests bench/watch_the_watchman.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/watch_the_watchman-3800e5adb30ef597)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/bounded_loops.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/bounded_loops-5ed478f3b2daff67)

running 1 test
test every_loop_names_the_constant_that_bounds_it ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/flush_contract.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/flush_contract-6e4d2fcabbb7d7b5)

running 3 tests
test a_commit_point_is_what_writes_an_on_commit_sink ... ok
test an_inline_sink_has_already_written_when_the_row_returns ... ok
test every_strategy_delivers_every_row_once_and_in_order ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s

     Running tests/otlp_roundtrip.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/otlp_roundtrip-9cc44f0de90ddab2)

running 1 test
test otlp_probe_spans_land_in_duckdb ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.34s

     Running tests/span_capture_linkage.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/span_capture_linkage-426bed272223f3f1)

running 1 test
test nested_span_counts_link_children_to_parents ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/span_chrome_trace.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/span_chrome_trace-54474d91f9abd7e7)

running 1 test
test nested_spans_land_in_chrome_trace ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/span_fanout_growth.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/span_fanout_growth-1edefd9f1d8b6641)

running 2 tests
test batched_maintenance_reads_as_constant_fanout ... ok
test per_row_maintenance_reads_as_linear_fanout ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/sqlite_statement_counters.rs (/Users/chrishafley/.agent/lanes/feature-watch-the-watchman/target/debug/deps/sqlite_statement_counters-544d5f72355678a5)

running 4 tests
test the_planner_account_names_the_temporary_btree ... ok
test an_unindexed_scan_reports_a_table_scan ... ok
test an_unindexed_order_by_reports_a_temporary_btree_sort ... ok
test an_indexed_lookup_reports_nothing ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests hafley_observe

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Fifteen tests pass, across nine test binaries and the doc-test binary:
`hafley_observe` 1, `watch_the_watchman` 0, `bounded_loops` 1,
`flush_contract` 3, `otlp_roundtrip` 1, `span_capture_linkage` 1,
`span_chrome_trace` 1, `span_fanout_growth` 2, `sqlite_statement_counters` 4.

Before this fix the same command died two ways: two `tracy-client-sys` rlibs
in one binary (`libtracy_client_sys-3cdaf498` and `libtracy_client_sys-f9c2dd6c`,
both carrying `TracyClient.o`) and then `signal: 11 (SIGSEGV)` from the
harness binary, which stopped the run before any integration test binary
executed. Failure mode 18 carries the RCA.

### R2: the table, from the one command

`just watch-the-watchman` built, measured and wrote the table. It is one
command, and the driver resumes: cells already measured are kept, so the table
below is the sum of two invocations. The first measured every candidate in nine
minutes fifty-two seconds, under the ceiling the brief sets. The second
re-measured the two sink candidates in two minutes seventeen seconds, because
their schema became idempotent in between and their rows had to be counted
again. Both invocations ran with `WATCH_BUDGET_SECONDS` above the ceiling;
the shipped default is six hundred seconds, and an invocation that runs out of
budget prints the table with `skipped` cells and names every one of them.

| feature | strategy | crates | binary bytes | wall off (ms, 3 runs) | wall on (ms, 3 runs) | pct cost | verdict |
|---|---|---|---|---|---|---|---|
| `fmt` | immediate | 0 | +236048 | 7.43,6.70,9.64 | 27.28,26.98,25.62 | +2.634 | cost |
| `fmt` | drain | 0 | +236048 | 6.07,10.44,10.61 | 26.09,25.48,26.06 | +1.496 | cost |
| `fmt` | on-commit | 0 | +236048 | 7.13,6.10,6.10 | 25.21,31.00,26.75 | +3.383 | cost |
| `chrome` | immediate | 4 | +828192 | 6.92,7.22,7.30 | 29.29,27.30,27.78 | +2.850 | cost |
| `chrome` | drain | 4 | +828192 | 6.62,6.21,7.28 | 26.40,29.22,29.24 | +3.414 | cost |
| `chrome` | on-commit | 4 | +828192 | 7.85,6.56,8.26 | 30.66,27.41,28.15 | +2.586 | cost |
| `otlp-trace` | immediate | 224 | +2078768 | 6.11,6.79,6.16 | 73.33,49.06,48.43 | +6.959 | cost |
| `otlp-trace` | drain | 224 | +2078768 | 6.21,6.00,6.05 | 76.49,56.55,50.42 | +8.340 | cost |
| `otlp-trace` | on-commit | 224 | +2078768 | 6.82,6.16,6.10 | 46.91,85.29,61.95 | +9.052 | cost |
| `otlp-metrics` | immediate | 3 | +749664 | 38.07,36.98,34.82 | 50.38,45.98,50.02 | +0.352 | cost |
| `otlp-metrics` | drain | 3 | +749664 | 38.30,36.10,35.91 | 44.92,43.45,46.45 | +0.244 | cost |
| `otlp-metrics` | on-commit | 3 | +749664 | 35.68,35.77,35.92 | 42.75,44.17,41.22 | +0.195 | cost |
| `sysmetrics` | immediate | 21 | +196752 | 43.91,46.04,42.22 | 46.26,48.36,44.83 | +0.053 | in the noise |
| `sysmetrics` | drain | 21 | +196752 | 44.51,42.43,40.88 | 46.24,43.94,44.77 | +0.055 | in the noise |
| `sysmetrics` | on-commit | 21 | +196752 | 47.08,45.48,43.34 | 43.11,42.66,45.14 | -0.052 | in the noise |
| `procmetrics` | immediate | 10 | +19552 | 44.08,44.57,43.58 | 46.53,47.40,50.16 | +0.076 | cost |
| `procmetrics` | drain | 10 | +19552 | 45.73,40.39,43.05 | 49.09,48.46,49.17 | +0.140 | cost |
| `procmetrics` | on-commit | 10 | +19552 | 44.32,42.20,45.10 | 47.84,51.93,43.06 | +0.079 | in the noise |
| `metrics-ctx` | immediate | 36 | +54640 | 44.80,46.64,45.41 | 53.88,52.57,53.70 | +0.183 | cost |
| `metrics-ctx` | drain | 36 | +54640 | 46.66,46.20,46.63 | 51.06,51.87,53.35 | +0.112 | cost |
| `metrics-ctx` | on-commit | 36 | +54640 | 46.35,49.18,50.33 | 52.93,53.00,52.08 | +0.076 | cost |
| `sqlite-sink` | immediate | 6 | +129088 | 6.38,6.08,6.48 | 10342.95,6471.18,6281.96 | +1012.656 | cost |
| `sqlite-sink` | drain | 6 | +129088 | 6.11,6.09,6.06 | 70.95,76.09,77.01 | +11.502 | cost |
| `sqlite-sink` | on-commit | 6 | +129088 | 5.95,6.05,6.12 | 57.98,55.87,56.92 | +8.405 | cost |
| `sqlite-sink-text` | immediate | 6 | +129088 | 6.61,6.49,6.20 | 8253.41,7302.06,6726.79 | +1124.646 | cost |
| `sqlite-sink-text` | drain | 6 | +129088 | 6.19,6.24,6.13 | 66.11,66.24,67.03 | +9.706 | cost |
| `sqlite-sink-text` | on-commit | 6 | +129088 | 6.16,6.77,6.01 | 65.19,57.31,58.98 | +8.582 | cost |
| `rusage` | immediate | 0 | +17328 | 8.97,6.89,6.38 | 18.99,27.57,20.19 | +1.929 | cost |
| `rusage` | drain | 0 | +17328 | 6.05,6.46,5.96 | 18.95,18.85,19.00 | +2.133 | cost |
| `rusage` | on-commit | 0 | +17328 | 6.06,6.16,6.08 | 19.80,19.26,20.09 | +2.256 | cost |
| `tracy` | immediate | 6 | +183680 | 6.12,6.07,5.97 | 17.66,17.75,17.43 | +1.910 | cost |
| `tracy` | drain | 6 | +183680 | 6.21,5.98,6.12 | 17.50,17.42,17.83 | +1.861 | cost |
| `tracy` | on-commit | 6 | +183680 | 6.15,6.14,5.89 | 17.76,18.39,17.92 | +1.918 | cost |
| `tracy-alloc` | immediate | 3 | +141632 | 6.11,6.10,6.04 | 6.42,5.99,6.04 | -0.011 | in the noise |
| `tracy-alloc` | drain | 3 | +141632 | 5.91,6.00,5.96 | 5.91,6.16,6.12 | +0.028 | in the noise |
| `tracy-alloc` | on-commit | 3 | +141632 | 6.03,6.00,6.08 | 6.17,6.00,6.32 | +0.022 | in the noise |

The complete table, every column, as written beside this document:

```tsv
feature	strategy	crates_added	binary_bytes	build_secs_off	build_secs_on	peak_rss_bytes	disk_write_bytes	disk_read_bytes	wall_ms_off	wall_ms_on	pct_cost	events_per_sec	verdict	notes
fmt	immediate	0	+236048	4.75,4.76,4.63	6.22,6.43,5.27	+622592	+0	+0	7.43,6.70,9.64	27.28,26.98,25.62	+2.634	741158	cost	formatter to a null writer, so formatting is priced and terminal I/O is not
fmt	drain	0	+236048	4.75,4.76,4.63	6.22,6.43,5.27	+622592	+0	+0	6.07,10.44,10.61	26.09,25.48,26.06	+1.496	767346	cost	formatter to a null writer, so formatting is priced and terminal I/O is not
fmt	on-commit	0	+236048	4.75,4.76,4.63	6.22,6.43,5.27	+688128	+0	+0	7.13,6.10,6.10	25.21,31.00,26.75	+3.383	747808	cost	formatter to a null writer, so formatting is priced and terminal I/O is not
chrome	immediate	4	+828192	4.35,4.87,4.58	5.50,5.69,5.50	+35028992	+0	+0	6.92,7.22,7.30	29.29,27.30,27.78	+2.850	719838	cost	timeline file under the lane target dir; the layer writes JSON from its own thread
chrome	drain	4	+828192	4.35,4.87,4.58	5.50,5.69,5.50	+34324480	+0	+0	6.62,6.21,7.28	26.40,29.22,29.24	+3.414	684408	cost	timeline file under the lane target dir; the layer writes JSON from its own thread
chrome	on-commit	4	+828192	4.35,4.87,4.58	5.50,5.69,5.50	+31490048	+0	+0	7.85,6.56,8.26	30.66,27.41,28.15	+2.586	710366	cost	timeline file under the lane target dir; the layer writes JSON from its own thread
otlp-trace	immediate	224	+2078768	4.64,4.62,4.51	5.88,5.61,5.65	+23953408	+0	+0	6.11,6.79,6.16	73.33,49.06,48.43	+6.959	407654	cost	no collector is listening, so the exporter's batches fail; the row prices the layer plus a failing exporter
otlp-trace	drain	224	+2078768	4.64,4.62,4.51	5.88,5.61,5.65	+26034176	+0	+0	6.21,6.00,6.05	76.49,56.55,50.42	+8.340	353700	cost	no collector is listening, so the exporter's batches fail; the row prices the layer plus a failing exporter
otlp-trace	on-commit	224	+2078768	4.64,4.62,4.51	5.88,5.61,5.65	+25624576	+0	+0	6.82,6.16,6.10	46.91,85.29,61.95	+9.052	322854	cost	no collector is listening, so the exporter's batches fail; the row prices the layer plus a failing exporter
otlp-metrics	immediate	3	+749664	5.43,5.76,5.68	6.00,5.94,6.03	+5603328	+0	+0	38.07,36.98,34.82	50.38,45.98,50.02	+0.352	399884	cost	same failing exporter on the metrics signal path
otlp-metrics	drain	3	+749664	5.43,5.76,5.68	6.00,5.94,6.03	+2211840	+0	+0	38.30,36.10,35.91	44.92,43.45,46.45	+0.244	445218	cost	same failing exporter on the metrics signal path
otlp-metrics	on-commit	3	+749664	5.43,5.76,5.68	6.00,5.94,6.03	+3457024	+0	+0	35.68,35.77,35.92	42.75,44.17,41.22	+0.195	467807	cost	same failing exporter on the metrics signal path
sysmetrics	immediate	21	+196752	5.98,6.07,5.89	6.17,6.23,6.28	+4210688	+0	+0	43.91,46.04,42.22	46.26,48.36,44.83	+0.053	432340	in the noise	the bought observer runs on its own thread and samples on an interval, so most of its work is off the workload's wall; the row reads the noise it adds
sysmetrics	drain	21	+196752	5.98,6.07,5.89	6.17,6.23,6.28	+3096576	+0	+0	44.51,42.43,40.88	46.24,43.94,44.77	+0.055	446738	in the noise	the bought observer runs on its own thread and samples on an interval, so most of its work is off the workload's wall; the row reads the noise it adds
sysmetrics	on-commit	21	+196752	5.98,6.07,5.89	6.17,6.23,6.28	+2342912	+0	+0	47.08,45.48,43.34	43.11,42.66,45.14	-0.052	463898	in the noise	the bought observer runs on its own thread and samples on an interval, so most of its work is off the workload's wall; the row reads the noise it adds
procmetrics	immediate	10	+19552	6.01,6.06,6.09	6.23,6.01,6.00	+1884160	+0	+0	44.08,44.57,43.58	46.53,47.40,50.16	+0.076	421902	cost	the bought process collector through the metrics facade, sampled on a bounded span cadence
procmetrics	drain	10	+19552	6.01,6.06,6.09	6.23,6.01,6.00	+3801088	+0	+0	45.73,40.39,43.05	49.09,48.46,49.17	+0.140	407429	cost	the bought process collector through the metrics facade, sampled on a bounded span cadence
procmetrics	on-commit	10	+19552	6.01,6.06,6.09	6.23,6.01,6.00	-4587520	+0	+0	44.32,42.20,45.10	47.84,51.93,43.06	+0.079	418070	in the noise	the bought process collector through the metrics facade, sampled on a bounded span cadence
metrics-ctx	immediate	36	+54640	6.04,5.76,6.14	7.67,6.74,6.25	+1622016	+0	+0	44.80,46.64,45.41	53.88,52.57,53.70	+0.183	372405	cost	the bought span-field label layer over the facade recorder
metrics-ctx	drain	36	+54640	6.04,5.76,6.14	7.67,6.74,6.25	+1409024	+0	+0	46.66,46.20,46.63	51.06,51.87,53.35	+0.112	385619	cost	the bought span-field label layer over the facade recorder
metrics-ctx	on-commit	36	+54640	6.04,5.76,6.14	7.67,6.74,6.25	-2359296	+0	+0	46.35,49.18,50.33	52.93,53.00,52.08	+0.076	377858	cost	the bought span-field label layer over the facade recorder
sqlite-sink	immediate	6	+129088	4.99,4.92,4.95	5.53,5.64,5.11	+3014656	+1461710848	+32768	6.38,6.08,6.48	10342.95,6471.18,6281.96	+1012.656	3091	cost	dictionary-encoded: repeated columns interned once, per-row values stored as they are
sqlite-sink	drain	6	+129088	4.99,4.92,4.95	5.53,5.64,5.11	+6717440	+8327168	+0	6.11,6.09,6.06	70.95,76.09,77.01	+11.502	262857	cost	dictionary-encoded: repeated columns interned once, per-row values stored as they are
sqlite-sink	on-commit	6	+129088	4.99,4.92,4.95	5.53,5.64,5.11	+5767168	+2109440	+0	5.95,6.05,6.12	57.98,55.87,56.92	+8.405	351388	cost	dictionary-encoded: repeated columns interned once, per-row values stored as they are
sqlite-sink-text	immediate	6	+129088	5.11,5.61,5.79	5.35,5.58,5.38	+4030464	+1455091712	+32768	6.61,6.49,6.20	8253.41,7302.06,6726.79	+1124.646	2739	cost	the same shape with every key inlined; the R4 control for the dictionary
sqlite-sink-text	drain	6	+129088	5.11,5.61,5.79	5.35,5.58,5.38	+8011776	+9654272	+8192	6.19,6.24,6.13	66.11,66.24,67.03	+9.706	301930	cost	the same shape with every key inlined; the R4 control for the dictionary
sqlite-sink-text	on-commit	6	+129088	5.11,5.61,5.79	5.35,5.58,5.38	+7471104	+3829760	+4096	6.16,6.77,6.01	65.19,57.31,58.98	+8.582	339117	cost	the same shape with every key inlined; the R4 control for the dictionary
rusage	immediate	0	+17328	4.86,4.84,5.23	4.80,4.83,5.58	-131072	+0	+0	8.97,6.89,6.38	18.99,27.57,20.19	+1.929	990477	cost	the sampler is compiled into every build, so this row prices the publishing layer alone
rusage	drain	0	+17328	4.86,4.84,5.23	4.80,4.83,5.58	-32768	+0	+0	6.05,6.46,5.96	18.95,18.85,19.00	+2.133	1055578	cost	the sampler is compiled into every build, so this row prices the publishing layer alone
rusage	on-commit	0	+17328	4.86,4.84,5.23	4.80,4.83,5.58	+16384	+0	+0	6.06,6.16,6.08	19.80,19.26,20.09	+2.256	1010097	cost	the sampler is compiled into every build, so this row prices the publishing layer alone
tracy	immediate	6	+183680	4.63,4.61,4.58	4.75,5.21,4.73	+20692992	+0	+0	6.12,6.07,5.97	17.66,17.75,17.43	+1.910	1132511	cost	tracy drops a span entered and exited on different threads, so its timeline is wrong under async
tracy	drain	6	+183680	4.63,4.61,4.58	4.75,5.21,4.73	+20774912	+0	+0	6.21,5.98,6.12	17.50,17.42,17.83	+1.861	1142748	cost	tracy drops a span entered and exited on different threads, so its timeline is wrong under async
tracy	on-commit	6	+183680	4.63,4.61,4.58	4.75,5.21,4.73	+20791296	+0	+0	6.15,6.14,5.89	17.76,18.39,17.92	+1.918	1115934	cost	tracy drops a span entered and exited on different threads, so its timeline is wrong under async
tracy-alloc	immediate	3	+141632	4.59,4.72,4.69	4.65,4.65,4.66	+2179072	+0	+0	6.11,6.10,6.04	6.42,5.99,6.04	-0.011	3312583	in the noise	the tracked global allocator from the same client the span layer uses; its cost follows the allocation count, and this workload allocates little
tracy-alloc	drain	3	+141632	4.59,4.72,4.69	4.65,4.65,4.66	+2195456	+0	+0	5.91,6.00,5.96	5.91,6.16,6.12	+0.028	3265550	in the noise	the tracked global allocator from the same client the span layer uses; its cost follows the allocation count, and this workload allocates little
tracy-alloc	on-commit	3	+141632	4.59,4.72,4.69	4.65,4.65,4.66	+2277376	+0	+0	6.03,6.00,6.08	6.17,6.00,6.32	+0.022	3243397	in the noise	the tracked global allocator from the same client the span layer uses; its cost follows the allocation count, and this workload allocates little
```

### R3: the three-run raw numbers

Every wall number is in the table above and in
`PLANS/watch-the-watchman.tsv`: three runs per side, per strategy. The
per-run rows, with the layer list each binary carried, the sampler readings and
the sink's row and byte counts, are the driver's raw file:

```
$CARGO_TARGET_DIR/watch-the-watchman/logs/raw.tsv
```

Each raw row is `candidate, side, run,` then the harness's own line: feature,
strategy, layers, sink, wall ms, peak rss bytes, disk read bytes, disk write
bytes, events, events per second, rows, database bytes, nanoseconds per event.
The driver refuses a run whose off side names a layer at all.

Two caveats on the sampler columns, both visible in the table:

- `peak_rss_bytes` is the kernel's high-water mark for the whole process, so a
  layer that starts threads (chrome, tracy, the OTLP exporters) shows its
  threads and a layer that does not can read negative against its own off side.
  A negative value is noise, not a saving.
- `disk_write_bytes` is the run's own delta between two `proc_pid_rusage`
  readings. The immediate sqlite rows pay it: one transaction per event is
  journal writes and commits, and the column shows the gigabytes that costs.

### R4: the dictionary sink against the all-TEXT sink

The two sinks write the same rows in the same order, so the comparison isolates
one variable: where the repeated text lives. Both write 80000 rows (20000
events, 60000 field rows) in every run.

| sink | rows | database bytes | wall ms, immediate | wall ms, drain | wall ms, on-commit |
|---|---|---|---|---|---|
| dictionary | 80000 | 1531904 | 10342.95, 6471.18, 6281.96 | 70.95, 76.09, 77.01 | 57.98, 55.87, 56.92 |
| all TEXT | 80000 | 3305472 | 8253.41, 7302.06, 6726.79 | 66.11, 66.24, 67.03 | 65.19, 57.31, 58.98 |

Read against each sink's own off side, from the table above: dictionary
`+1012.6%`, `+11.5%`, `+8.4%`; all TEXT `+1124.6%`, `+9.7%`, `+8.6%`.

Three readings, none of them rounded away:

- On disk the dictionary wins decisively: the same rows in 1.5 MB against
  3.3 MB, a factor of 2.16, at every strategy.
- On the drain wall the dictionary loses. Its three on-runs (70.95, 76.09,
  77.01) sit entirely above the control's (66.11, 66.24, 67.03), a gap of
  about ten milliseconds against an off wall of about six. Against each sink's
  own off side that is `+11.5%` for the dictionary and `+9.7%` for the control.
- On the immediate and on-commit walls the two overlap and the difference is
  not readable: immediate is dominated by one transaction per event, and
  on-commit is a wash.

So the measured rule holds on bytes and does not hold on the drain wall at this
volume. The dictionary pays for two extra btree probes per new key and carries
five side tables; at 80k rows the page work dominates key width, which is the
same conclusion the sibling repo recorded for packed keys on a pure insert.
The interning bill is small because the keys repeat, and the disk bill is large
because they do not.

The dictionary stays, and the reason is not the wall: a log table whose keys
repeat once per row is a denormalized table, and the design law in this repo
forbids it. The three runs above are the price of that law at this scale.

### R5: clippy

```
$ cargo clippy -p hafley-observe --all-targets --all-features
0 warnings, 0 errors
$ cargo clippy -p hafley-observe --all-targets
0 warnings, 0 errors
```

Both runs cover the library, the harness binary, the example and every test
target. The crate's own tests pass under the default feature set:

```
test result: ok. 4 passed   (sqlite_statement_counters)
test result: ok. 3 passed   (flush_contract)
test result: ok. 2 passed   (span_fanout_growth)
test result: ok. 1 passed   (bounded_loops)
test result: ok. 1 passed   (span_capture_linkage)
test result: ok. 1 passed   (span_chrome_trace)
test result: ok. 1 passed   (otlp_probe_spans_land_in_duckdb)
```

### R6: pre-existing red legs

Recorded before the first edit, in a detached worktree at `3660640` whose
working tree this lane never touched:

```
$ git worktree add --detach <lane>/baseline HEAD
$ cd <lane>/baseline && cargo check --workspace --all-targets --locked
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 22s
```

Exit 0. The only output was warnings, eight of them from
`crates/redux/examples/_4_machine_macro.rs`, which is a pre-existing
dead-code warning set and not a red leg. There is no compile-level red leg in
this workspace at the base commit.

After the edits, the same command in this worktree:

```
$ cargo check --workspace --all-targets --locked
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 37.34s
```

Exit 0, same warnings, no new ones. `--locked` holds, so the workspace lock
resolved to a complete file.

The test-level red legs are the ones the `hafley-rs-repo` skill records, and
this lane did not run them: the boop lane, temp-home and live-harness set fails
environmentally on this machine while other agents hold live worktrees, and the
sprefa-extract golden parity pair fails on 11 oracle files that carry a stale
root prefix. Both sets belong to other lanes. Their signatures, from the skill,
are `crates/boop/tests/lane_carcass.rs`,
`crates/boop/tests/5_live_harness.rs`, `crates/boop/tests/temp_home_rail.rs`
and `tests/golden_parity.rs::ported_facets_match_v5` /
`::rust_doc_parity`. Unchanged before and after this lane's work by
construction: this lane's diff touches one crate, one recipe and one doc.

### R7: diff scope

```
$ git diff --name-only origin/main...HEAD
Cargo.lock
crates/hafley-observe/Cargo.toml
crates/hafley-observe/PLANS/2026-09-21-watch-the-watchman.md
crates/hafley-observe/PLANS/2026-09-21-watch-the-watchman.visual.human.unga.md
crates/hafley-observe/PLANS/watch-the-watchman.tsv
crates/hafley-observe/bench/watch_the_watchman.rs
crates/hafley-observe/bench/watch_the_watchman.sh
crates/hafley-observe/bench/watch_the_watchman_report.py
crates/hafley-observe/examples/otlp_probe.rs
crates/hafley-observe/src/0_types.rs
crates/hafley-observe/src/10_tracy.rs
crates/hafley-observe/src/1_format.rs
crates/hafley-observe/src/1_init.rs
crates/hafley-observe/src/2_otlp.rs
crates/hafley-observe/src/3_chrome.rs
crates/hafley-observe/src/5_sqlite.rs
crates/hafley-observe/src/6_flush.rs
crates/hafley-observe/src/7_sink.rs
crates/hafley-observe/src/8_rusage.rs
crates/hafley-observe/src/9_metrics.rs
crates/hafley-observe/src/lib.rs
crates/hafley-observe/tests/bounded_loops.rs
crates/hafley-observe/tests/flush_contract.rs
crates/hafley-observe/tests/otlp_roundtrip.rs
crates/hafley-observe/tests/span_chrome_trace.rs
crates/hafley-observe/tests/sqlite_statement_counters.rs
docs/failure-modes.md
justfile
```

No line counts appear in this receipt. The receipts page is one of the files
listed, so a count written into it changes the count it states, and four
commits went nowhere proving that. The file list does not have that property.

Every path is owned by this lane: `crates/hafley-observe` (sources, manifest,
bench, tests, example, plans) and the one recipe added to `justfile`. The root
`Cargo.lock` changed because the crate gained dependencies, which the brief
allows. One file outside that list is present by the brief's own law: the
`docs/failure-modes.md` rows for the incidents that bit this lane.

The workspace `Cargo.lock` was regenerated by cargo, not hand-merged, and
`cargo check --workspace --all-targets --locked` above holds on it.

Every path is owned by this lane: `crates/hafley-observe` (sources, manifest,
bench, tests, example, plans) and the one recipe added to `justfile`. The root
`Cargo.lock` changed because the crate gained dependencies, which the brief
allows. One file outside that list is present by the brief's own law: the row
`docs/failure-modes.md` gained for the off-side defect, because every incident
that bites gets a row there.

The workspace `Cargo.lock` was regenerated by cargo, not hand-merged, and
`cargo check --workspace --all-targets --locked` above holds on it.

### R8: the bounded-loop scanner

```
$ cargo test -p hafley-observe --test bounded_loops -- --nocapture
1 bounded loops:
6_flush.rs:283 loop { <- // budget: DRAIN_WAIT per wait, DRAIN_BATCH_BOUND rows per write
```

The scanner walks `src`, classifies `loop`, `while` and `loop {`, requires a
budget comment within six lines above each one, and requires the budget to name
a constant the same file declares. It fails if a loop has no budget line, if a
budget line names no declared constant, or if it finds no loops at all, so a
scanner that stops seeing loops fails instead of passing.

The crate holds exactly one `loop`, the drain, and no recursion. Every other
repetition is a `for` over a constant bound or over a bounded channel, listed
in the plain-words twin.

### R9: the plain-words twin

`PLANS/2026-09-21-watch-the-watchman.visual.human.unga.md` carries the same
findings as row tables and one-line-per-edge trees, with no mermaid.