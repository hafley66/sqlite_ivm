# watch the watchman, in plain words

The same findings as `2026-09-21-watch-the-watchman.md`, written for a reader
who wants sentences and row tables. No diagram markup.

## The question

Tracing costs something. Which tracing layer costs what, and what does a sink
cost when it writes immediately, when a background drain writes it, and when
the host names the commit point?

## The answer, in one line each

| layer | what it costs, in words |
|---|---|
| `fmt` | formats every event into a line and drops it, which is the text baseline |
| `chrome` | appends a timeline record per span from its own thread |
| `otlp-trace` | queues a span batch and fails to post it, because nothing is listening |
| `otlp-metrics` | the same, on the metrics signal path, plus a span counter and a duration histogram |
| `sysmetrics` | runs the bought system observer on its own runtime and records into a meter |
| `procmetrics` | reads the process on a bounded span cadence through the bought collector |
| `metrics-ctx` | copies span fields into metric labels for every instrument the facade records |
| `tracy` | walks a callstack at every zone, and its timeline is wrong under async |
| `tracy-alloc` | reports every allocation in the process, from the same client as the span layer |
| `rusage` | reads the kernel counters at every span close and emits a record |
| `sqlite-sink` | interns the repeating columns and writes a transaction per batch, or per event when immediate |

The measured numbers live in the row tables below.

## How a row is built

Each candidate is built twice from the same source. The off build has no
layers, or only the smaller set the candidate is priced on top of. The on build
has the candidate and that same smaller set. Both run the same workload three
times per strategy. The table keeps the three walls on each side and the
median change between them.

The cell reads `cost` only when every on-run sits outside the spread of the
off-runs. Otherwise the cell reads `in the noise`, and the six raw numbers are
still printed.

## What the layers are wired through

One line per edge. Left of the arrow is the source, right is the target.

```
just watch-the-watchman -> bench/watch_the_watchman.sh
bench/watch_the_watchman.sh -> cargo build (off set) -> watch-the-watchman binary
bench/watch_the_watchman.sh -> cargo build (on set) -> watch-the-watchman binary
bench/watch_the_watchman.sh -> cargo tree -p hafley-observe -> node count
bench/watch_the_watchman.sh -> strip -S -x -o -> stripped artifact -> byte count
bench/watch_the_watchman.sh -> watch-the-watchman (3 runs) -> raw rows
bench/watch_the_watchman.sh -> watch_the_watchman_report.py -> table + tsv
watch_the_watchman_report.py -> PLANS/watch-the-watchman.tsv
```

Inside one harness run:

```
main -> instruments::install -> facade recorder over the meter provider
main -> instruments::start -> bought system observer thread
main -> stack() -> registry + every enabled layer in one chain
stack() -> format_layer -> text or json line, null writer
stack() -> chrome_layer -> timeline file
stack() -> context_layer -> span fields as metric labels
stack() -> span_layer -> span counter and duration histogram
stack() -> proc_layer -> bought process collector
stack() -> tracy_layer -> sampled callstacks
stack() -> rusage_layer -> usage record per span close
stack() -> otlp_layer -> batch span exporter
stack() -> SinkLayer -> Writer -> Flush strategy -> LogSink -> sqlite
workload() -> nested spans -> one event per innermost span
```

Inside the writer:

```
Writer::write --> immediate --> sink.write(one row)
Writer::write --> drain --> bounded channel --> drain thread --> sink.write(batch)
Writer::write --> on-commit --> buffer --> flush() --> sink.write(batch)
drain thread --> bound hit --> named diagnostic --> emitting thread writes inline
```

## The sink, in plain words

Field names, span names, targets, file paths and levels repeat on nearly every
record. The row identity does not. The dictionary sink keeps each repeating
column in its own table with an integer key, and the event row carries the
key. The values that do not repeat, such as field values and timestamps, are
stored as they are.

The control sink keeps the same two tables and inlines every key. The two
sinks write the same rows in the same order, so the comparison isolates one
variable: where the repeated text lives.

Interning is cached, and every row pays a hash lookup at worst. The side
tables are append-only and their unique constraint is the dedup.

## The relational laws this sink follows

| law | how the sink follows it |
|---|---|
| integer surrogate keys | every event and value row keys on an integer |
| a natural key is stored once | span name, target, file, level and field name each have one table with `UNIQUE` |
| no composite text primary key | the value table keys on two integers |
| atomic columns | each field value is its own row, not a joined string |
| booleans are integers | no boolean column exists in the log schema |
| ids carry no meaning | ids are dense integers, never parsed |
| readable output is a join | a reader joins the dictionaries at the read boundary |

## The bounded loops

One line per repetition, with the constant that bounds it. The scanner test prints
its own version of this table for `loop` and fails when a loop has no budget.

```
src/6_flush.rs:240   for _ in 0..FLUSH_WAIT_STEPS        <- FLUSH_WAIT_STEPS
src/6_flush.rs:283   loop {                               <- DRAIN_WAIT, DRAIN_BATCH_BOUND
src/6_flush.rs:299   for row in rows.try_iter()           <- DRAIN_ROW_BOUND
src/5_sqlite.rs:255  for row in rows                     <- the batch the caller passed
src/5_sqlite.rs:279  for row in rows                     <- the batch the caller passed
src/9_metrics.rs:437 for _ in 0..OBSERVER_SAMPLES         <- OBSERVER_SAMPLES
tests/bounded_loops.rs:17 while let Some(directory) = pending.pop() <- one entry per push
```

The scanner test reports the one `loop` and its budget line; the `for` and
`while` lines are bounded by the constant or the collection they name.

## Findings

In plain words, from the table:

| layer | what it did to the event path |
|---|---|
| the text formatter | doubled it, and tripled it on one strategy, and is still the cheapest layer here |
| the chrome timeline | multiplied it by about three and a half, and added tens of megabytes resident |
| the OTLP span exporter | multiplied it by seven to nine, plus twenty odd megabytes resident |
| the OTLP metrics pipeline | added a fifth to a third on top of the trace exporter it needs |
| the tracing context layer | added a tenth to a fifth |
| the process collector | added a tenth, sampled on a cadence |
| the system observer | read as noise, because it samples on its own thread |
| the Tracy zone layer | nearly doubled the path |
| the tracked allocator | read as noise, because the workload allocates little |
| the usage layer | multiplied the path by about two to two and a quarter |
| the relational sink | see the two sink rows below |

The flush strategy moves the sink, not the layer:

| strategy | what it buys |
|---|---|
| immediate | every event is its own transaction, and the sink costs ten times the workload |
| drain | one batch per drain pass, and the sink costs about a tenth on top of the workload |
| on-commit | the same, slightly cheaper, because the host names one commit point |

The two sink rows, compared against each other:

| sink | rows | database bytes | drain wall, three runs |
|---|---|---|---|
| dictionary | 80000 | 1531904 | 70.95, 76.09, 77.01 |
| all TEXT | 80000 | 3305472 | 66.11, 66.24, 67.03 |

The dictionary keeps the disk to about half and loses about ten milliseconds of
the drain wall at this volume, against an off wall of about six. It stays,
because repeating a key once per row is denormalization and the design law
forbids the control shape.

What this means for a host: turn a layer on when its question is worth a
multiple of the event path. The formatter and the usage record are the cheap
ones, the zone and exporter layers are the expensive ones, and the sink's
strategy matters more than the sink's schema.

## What is not measured

A collector is not running, so the OTLP rows price a failing exporter rather
than a delivered batch. The `fmt` row writes to a null writer, so terminal I/O
is outside it. The `rusage` row prices its publishing layer, because the
sampler itself is compiled on both sides. Tracy's timeline is wrong under
async by construction, so its row is a lower bound on what a correct
cross-thread profiler would cost.