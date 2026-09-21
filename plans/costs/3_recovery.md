# SQLite performance recovery

Main integration includes the existing batch-boundary changes, vendored hafley-rs
crates from `1fbf655a`, shared event statistics in hafley-observe, executable native
loading tests, and isolated extension builds.

Inner joins now propagate signed deltas in left-before-update / right-after-update
order. Arrangement updates and cleanup use changed row hashes, retaining exact
identity checks. Outer joins, groups, sets, and recursive semantics retain their
existing maintenance paths. One source transaction still owns one atomic drain.

Validation: 69 root tests passed, including a new bag oracle for both-side updates,
self joins, residual predicates, null keys, cancellation, and rollback. Three
native extension tests, CRUD, and CLI compass passed. The quiet verification rerun
passed the existing timing thresholds. The first run exceeded one short-test
threshold (0.276 s versus 0.246 s ceiling); no timing baseline was relaxed.
The quick shootout produced 240 checksum-valid totals, including warmups, across
20 circuits and three engines. Historical crossover replay checks all five states
in each of nine cases, three repetitions, for both engines.

| 12000 rows / batch 1000 / fanout 200 | SQLite ms | DD ms | SQLite/DD |
|---|---:|---:|---:|
| Historical counted arm | 21.121 | 1.118 | 18.89 |
| Recovered fixture, current plugin before join changes | 197.916 | 1.175 | 168.39 |
| Signed join deltas | 94.994 | 1.117 | 85.03 |
| Changed-hash arrangement bounds | 91.396 | 1.091 | 83.78 |

The historical performance target remains unmet. The historical arm is a counted
query-specific implementation; the current plugin uses general relational
arrangements. The fixture and DD consumer were recovered unchanged from sprefa
`e2052d5ae`. SQLite uses WAL/FULL and DD remains volatile. Historical results were
recorded in an earlier invocation; the table does not establish a same-host paired
comparison with the historical binary.

Profiling the signed-join version attributed 19 ms to eight aggregate recomputations
and 18 ms to full arrangement UPDATE scans in the batch-1000 workload. The latter
scans are bounded in the final change. Aggregate before/after recomputation and
canonical row encoding remain in this path.

[Retained replay metadata and results](2_crossover_recovery.json) include invocation
time, extension/DD hashes, and every per-case timing sample. Local raw process
receipts are under `bench/results/recovery-crossover*`.

[Benchmark and lab inventory](0_benchmark_inventory.md) includes source ages and
copyable commands. Historical labs have not been rerun unless their row says so.
The original dirty worktrees remain intact.
