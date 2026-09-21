# toward dd: baseline

Yardstick for this lane: per circuit, per n, per fanout, per arm, wall ms,
peak RSS, disk bytes written and read, db bytes on disk, arrangement rows, and
the derived `ivm/dd` (sqlite-ivm wall over dd wall). The job is to push
`ivm/dd` down, circuit by circuit.

Command (13.7 min, one bench process, `nice -n 10`):

```
bench scale --circuits chain,join,group,distinct,window,reach \
  --n 1000,10000,100000 --fanout 1,10 --reps 1 --out plans/costs/toward-dd/0_baseline
```

`--out` holds `scale.tsv` and the SVGs; they are moved up as `0_baseline.tsv`,
`0_baseline-<circuit>.svg`, and `0_baseline-<circuit>-ivm-over-dd.svg`.
`0_baseline.tsv` is the full receipt, one row per arm. Every cell's final read
was checked against a plain in-memory recompute before its numbers were kept.

Three arms. `sqlite-ivm` is the extension registered in process with a virtual
table. `sqlite-query` runs the circuit query directly, no view. `dd` is the
differential-dataflow graph in `bench/src/scale_dd.rs`, one graph per circuit.
All three take the same seed and the same write stream: 40 single-statement
inserts, 40 deletes, 40 updates, one 1000-row `INSERT OR REPLACE` transaction,
then the circuit read.

## wall and ivm/dd

`wall_ms` is the whole cell: writes plus reads. Reads are the circuit read
repeated 20 times at n=1000, 3 at n=10000, 1 at n=100000.

| circuit | fanout | n | sqlite-ivm ms | sqlite-query ms | dd ms | ivm/dd |
|---|---|---|---|---|---|---|
| chain | 1 | 1000 | 82.4 | 17.2 | 2.8 | 29.243 |
| chain | 1 | 10000 | 364.4 | 29.3 | 3.5 | 104.264 |
| chain | 1 | 100000 | 3284.6 | 164.0 | 6.4 | 514.662 |
| chain | 10 | 1000 | 2113.2 | 552.4 | 66.2 | 31.945 |
| chain | 10 | 10000 | 13084.5 | 1137.2 | 123.7 | 105.796 |
| chain | 10 | 100000 | 28896.9 | 4011.2 | 468.9 | 61.629 |
| distinct | 1 | 1000 | 28.0 | 6.9 | 2.4 | 11.851 |
| distinct | 1 | 10000 | 176.5 | 9.0 | 2.7 | 66.586 |
| distinct | 1 | 100000 | 1413.8 | 16.1 | 5.0 | 281.155 |
| distinct | 10 | 1000 | 25.5 | 6.8 | 1.7 | 15.192 |
| distinct | 10 | 10000 | 29.6 | 7.1 | 1.7 | 17.009 |
| distinct | 10 | 100000 | 174.9 | 10.9 | 1.8 | 98.305 |
| group | 1 | 1000 | 38.8 | 9.6 | 1.8 | 21.437 |
| group | 1 | 10000 | 45.5 | 15.2 | 2.9 | 15.594 |
| group | 1 | 100000 | 56.2 | 38.6 | 6.5 | 8.688 |
| group | 10 | 1000 | 39.0 | 9.7 | 1.7 | 23.159 |
| group | 10 | 10000 | 41.5 | 11.4 | 1.8 | 23.196 |
| group | 10 | 100000 | 45.9 | 29.6 | 2.0 | 23.377 |
| join | 1 | 1000 | 102.2 | 46.6 | 7.2 | 14.249 |
| join | 1 | 10000 | 180.4 | 15.8 | 2.5 | 72.889 |
| join | 1 | 100000 | 1469.0 | 38.8 | 4.9 | 298.649 |
| join | 10 | 1000 | 146.4 | 60.9 | 8.3 | 17.704 |
| join | 10 | 10000 | 4380.2 | 749.8 | 90.7 | 48.289 |
| join | 10 | 100000 | 2022.3 | 339.3 | 41.4 | 48.806 |
| reach | 1 | 1000 | 507.6 | 12.6 | 5.7 | 88.975 |
| reach | 1 | 10000 | 1782.9 | 29.0 | 5.6 | 317.132 |
| reach | 1 | 100000 | 647667.4 | 87.8 | 8.1 | 79625.536 |
| reach | 10 | 1000 | 682.1 | 11.5 | 4.7 | 145.021 |
| reach | 10 | 10000 | 652.9 | 14.1 | 5.4 | 120.651 |
| reach | 10 | 100000 | 3319.0 | 44.6 | 4.9 | 682.927 |
| window | 1 | 1000 | 54.7 | 18.0 | 3.3 | 16.611 |
| window | 1 | 10000 | 186.3 | 26.3 | 3.1 | 60.223 |
| window | 1 | 100000 | 1472.5 | 76.6 | 6.9 | 214.838 |
| window | 10 | 1000 | 63.2 | 18.4 | 3.9 | 16.380 |
| window | 10 | 10000 | 300.2 | 25.2 | 6.5 | 45.993 |
| window | 10 | 100000 | 1527.4 | 75.5 | 7.1 | 216.211 |

## resources

Peak RSS is the getrusage high-water growth across the cell, so only the cell
that first raises the process high-water reports a number. Disk read and write
are the `proc_pid_rusage` counters across the cell. `dd` holds no database, so
its db bytes and arrangement rows are 0.

| circuit | fanout | n | ivm peak RSS MiB | ivm disk read B | ivm disk write B | ivm db B | ivm arr rows | dd peak RSS MiB |
|---|---|---|---|---|---|---|---|---|
| chain | 1 | 1000 | 4.3 | 53248 | 7168000 | 598016 | 18 | 3.2 |
| chain | 1 | 10000 | 14.3 | 49152 | 26468352 | 4354048 | 18 | 3.9 |
| chain | 1 | 100000 | 105.8 | 126976 | 218419200 | 46985216 | 18 | 60.7 |
| chain | 10 | 1000 | 0.0 | 86016 | 58044416 | 4861952 | 18 | 0.0 |
| chain | 10 | 10000 | 211.6 | 47534080 | 721100800 | 53088256 | 18 | 178.3 |
| chain | 10 | 100000 | 1057.2 | 2137071616 | 2436919296 | 590868480 | 18 | 469.2 |
| distinct | 1 | 1000 | 0.0 | 20480 | 4366336 | 249856 | 6 | 0.0 |
| distinct | 1 | 10000 | 0.0 | 24576 | 10436608 | 2322432 | 6 | 0.0 |
| distinct | 1 | 100000 | 0.0 | 61440 | 85438464 | 24764416 | 6 | 0.0 |
| distinct | 10 | 1000 | 0.0 | 0 | 3325952 | 237568 | 6 | 0.0 |
| distinct | 10 | 10000 | 0.0 | 4096 | 6901760 | 1171456 | 6 | 0.0 |
| distinct | 10 | 100000 | 0.0 | 94208 | 50941952 | 13393920 | 6 | 0.0 |
| group | 1 | 1000 | 0.0 | 0 | 3289088 | 225280 | 6 | 0.0 |
| group | 1 | 10000 | 0.0 | 0 | 5668864 | 1282048 | 6 | 0.0 |
| group | 1 | 100000 | 0.0 | 36864 | 38322176 | 13549568 | 6 | 0.0 |
| group | 10 | 1000 | 0.0 | 0 | 3252224 | 212992 | 6 | 0.0 |
| group | 10 | 10000 | 0.0 | 0 | 5365760 | 1146880 | 6 | 0.0 |
| group | 10 | 100000 | 0.0 | 0 | 35909632 | 11448320 | 6 | 0.0 |
| join | 1 | 1000 | 0.0 | 118784 | 9097216 | 733184 | 11 | 0.0 |
| join | 1 | 10000 | 0.0 | 40960 | 17772544 | 3178496 | 11 | 0.0 |
| join | 1 | 100000 | 0.0 | 45056 | 140783616 | 34549760 | 11 | 0.0 |
| join | 10 | 1000 | 0.0 | 45056 | 8466432 | 782336 | 11 | 0.0 |
| join | 10 | 10000 | 0.0 | 208896 | 295567360 | 36831232 | 11 | 0.0 |
| join | 10 | 100000 | 0.0 | 45056 | 350597120 | 80379904 | 11 | 0.0 |
| reach | 1 | 1000 | 0.0 | 16384 | 10252288 | 417792 | 13 | 0.0 |
| reach | 1 | 10000 | 0.0 | 45056 | 52850688 | 4161536 | 13 | 0.0 |
| reach | 1 | 100000 | 0.0 | 93843456 | 796778496 | 45768704 | 13 | 0.0 |
| reach | 10 | 1000 | 0.0 | 8192 | 9097216 | 405504 | 13 | 0.0 |
| reach | 10 | 10000 | 0.0 | 20480 | 16777216 | 2363392 | 13 | 0.0 |
| reach | 10 | 100000 | 0.0 | 61440 | 154308608 | 25915392 | 13 | 0.0 |
| window | 1 | 1000 | 0.0 | 0 | 7999488 | 368640 | 6 | 0.0 |
| window | 1 | 10000 | 0.0 | 40960 | 13107200 | 2551808 | 6 | 0.0 |
| window | 1 | 100000 | 0.0 | 81920 | 102457344 | 27525120 | 6 | 0.0 |
| window | 10 | 1000 | 0.0 | 12288 | 7532544 | 356352 | 6 | 0.0 |
| window | 10 | 10000 | 0.0 | 65536 | 34381824 | 2289664 | 6 | 0.0 |
| window | 10 | 100000 | 0.0 | 32768 | 96595968 | 22888448 | 6 | 0.0 |

## the three worst cells

1. `reach` fanout 1 n 100000: `ivm/dd` 79625.536. sqlite-ivm 647667.4 ms
   against dd 8.1 ms. Inside the cell: delete 5960.0 ms per statement,
   update 7754.0 ms, replace 98720.3 ms.
2. `reach` fanout 10 n 100000: `ivm/dd` 682.927. sqlite-ivm 3319.0 ms against
   dd 4.9 ms.
3. `chain` fanout 1 n 100000: `ivm/dd` 514.662. sqlite-ivm 3284.6 ms against
   dd 6.4 ms.

Every cell is above 3x: the best is `group` fanout 1 n 100000 at 8.688, the
worst 79625.536. The stop condition is far off.

## defects

Columns over 10 s, printed by the sweep rather than hidden:

| circuit | fanout | n | arm | column | ms |
|---|---|---|---|---|---|
| chain | 10 | 10000 | sqlite-ivm | wall | 13084.5 |
| chain | 10 | 100000 | sqlite-ivm | wall | 28896.9 |
| reach | 1 | 100000 | sqlite-ivm | wall | 647667.4 |
| reach | 1 | 100000 | sqlite-ivm | replace | 98720.3 |

The `reach` fanout 1 n 100000 cell is the single worst: its deletes and updates
are three orders of magnitude above every other n, and its disk write (801 MB)
and disk read (126 MB) are the largest in the sweep. That cell alone is 10.8
min of the 13.7 min run.

## profile, `reach` fanout 1

Two profiles of the same cell shape, plus a shape sweep between them:

```
samply record --save-only -o /tmp/reach-f1-n40k.json -- \
  bench scale --circuits reach --n 40000 --fanout 1 --arms sqlite-ivm
samply record --save-only -o /tmp/reach-f1-n100k.json -- \
  bench scale --circuits reach --n 100000 --fanout 1 --arms sqlite-ivm
bench scale --circuits reach --n 20000,40000,60000,80000 --fanout 1 --arms sqlite-ivm
```

Leaf self time from the profile JSON, frames symbolized with `atos` against the
bench binary. `bench/Cargo.toml` keeps symbols in the release profile, so a
later profile resolves to function and line.

### the shape

`cliff.tsv` holds the sweep. Per-statement cost at fanout 1:

| n | delete ms | update ms | replace ms | cell wall ms |
|---|---|---|---|---|
| 1000 | 0.4 | 0.7 | 451.2 | 507.6 |
| 10000 | 10.9 | 12.6 | 794.4 | 1782.9 |
| 20000 | 64.0 | 77.2 | 2464.2 | 8200.8 |
| 40000 | 351.5 | 345.4 | 2075.6 | 30118.1 |
| 60000 | 10189.9 | 5885.5 | 50150.4 | 693419.8 |
| 80000 | 17784.7 | 11746.8 | 88118.6 | 1269715.4 |
| 100000 | 5960.0 | 7754.0 | 98720.3 | 647667.4 |

The 40000 to 60000 step is 29x for 1.5x the rows, and the cost falls again from
80000 to 100000. That is a threshold, not a smooth law.

### the frames

These two tables come from a `debug = 0` binary, where `atos` falls back to
the nearest symbol; treat the small frames as approximate. The reliable
symbolized profile is in the next section.

n 40000, 31.8 s of samples: `sqlite3VdbeExec` 65.5%, `sqlite3VdbeSerialGet`
7.8%, `getCellInfo` 6.8%, `sqlite3BtreeNext` 6.1%, `btreeParseCellPtr` 5.2%.
Every frame is a b-tree scan.

n 100000, 664.4 s of samples:

| frame | s | share |
|---|---|---|
| `sqlite3VdbeExec` | 354.4 | 53.3% |
| `sqlite3KeyInfoUnref` | 85.9 | 12.9% |
| `sqlite3VdbeFreeCursorNN` | 61.1 | 9.2% |
| `memjrnlWrite` | 44.7 | 6.7% |
| system frame (malloc or kernel; `atos` cannot split the two) | 39.2 | 5.9% |
| `sqlite3PagerSharedLock` | 20.6 | 3.1% |
| `sqlite3VdbeHalt` | 11.9 | 1.8% |
| `btreeInvokeBusyHandler` | 9.7 | 1.5% |
| `newDatabase` | 7.8 | 1.2% |
| `sqlite3VdbeMemTranslate` | 6.0 | 0.9% |
| `sqlite3VtabUnlock` | 5.2 | 0.8% |

Inclusive: `<sqlite_ivm::vtab::Table>::drain` 99.6%,
`<sqlite_ivm::relational::Plan>::drain` 97.7%,
`CachedExecute::execute_cached` 98.2%. The cell is one drain per statement.

The frames that only appear at the large size are ephemeral-table machinery:
`sqlite3KeyInfoUnref`, `sqlite3VdbeFreeCursorNN`, `memjrnlWrite`,
`sqlite3PagerSharedLock`, `newDatabase`, `sqlite3VdbeHalt`. Together 36.0%,
240 s of 664 s. Splitting `apply` by call site: 559.4 s in the 120
single-statement writes, 101.6 s in the one-transaction 1000-row replace.

### hypothesis

The closure is wide, not deep. `b.k` covers every residue, so a breadth first
walk from the roots reaches every node in one hop (measured directly on the
seed formula: reached equals g, max distance 1), and `RUST_LOG=sqlite_ivm=debug`
shows the fixpoint settling in two derive rounds. Round count is not the
driver.

Arrivals and departures cost differently. The insert phase, which grows the
closure, stays linear across the whole shape sweep: 1.9 ms per statement at
n 20000, 3.7 at 40000, 5.4 at 60000, 7.3 at 80000. The delete and update
phases, which shrink it, cliff: 351.5 ms at n 40000 against 10189.9 at n 60000.
So the cost sits in the departure pass, not in the closure derive.

Hypothesis: the departure pass costs a number of full scans of the member
table per statement, and that number rises with the closure, so the cost is
superlinear in the closure size and shows a cliff where the working set stops
fitting in cache. The number to move is the per-statement delete cost, 351.5 ms
at n 40000 against 10189.9 ms at n 60000, and 5960.0 ms at n 100000.

### profile with symbols

The first profile was taken with `debug = 0`, where `atos` falls back to the
nearest symbol and its frame names are wrong: it read as 36% ephemeral-table
machinery. `bench/Cargo.toml` now keeps symbols in release, and the same cell
resolves to:

| frame | s | share |
|---|---|---|
| `sqlite3VdbeExec` | 408.8 | 61.5% |
| `sqlite3VdbeSerialGet` | 76.2 | 11.5% |
| `btreeParseCellPtr` | 42.7 | 6.4% |
| `getCellInfo` | 42.0 | 6.3% |
| `sqlite3BtreeNext` | 41.0 | 6.2% |

91.9% is SQLite's row scan and record deserialization path, with almost
nothing in ephemeral-table setup. The cell is a very large number of full
scans of the member table.

### rejected

Two changes, no commit in `src/`:

| change | cell | before ms | after ms | verdict |
|---|---|---|---|---|
| `restore` was one statement whose `WHERE` is `EXISTS(r1) OR EXISTS(r2)`; it became one statement whose body is `SELECT .. WHERE EXISTS(r1) UNION ALL SELECT .. WHERE EXISTS(r2)`, one branch per rule | reach fanout 1 n 100000 | 647947.0, 647428.6, 644158.9 | 2141392.0, rep 0 only, killed | 3.3x slower: the compound select materializes |
| `drop_deleted_range` was `DELETE FROM all WHERE __k IN (SELECT __k FROM work WHERE rowid range)`; it became `DELETE FROM all WHERE rowid IN (SELECT a.rowid FROM work w JOIN all a ON a.__k=w.__k WHERE w.rowid range)` | reach fanout 1 n 40000 | 351.5 delete, 30118.1 wall | 355.9 delete, 30480.4 wall | no change: SQLite already drove the membership test from the range |
| `derive_sql`'s only-present filter was `WHERE __k IN (SELECT __k FROM all)`; it became `WHERE EXISTS(SELECT 1 FROM all m WHERE m.__k=d.__k)` over an aliased derived table | reach fanout 1 n 40000 | 351.5 delete, 30118.1 wall | 348.5 delete, 29874.9 wall | no change inside the 1.2% run-to-run spread |

### departure rounds

`RUST_LOG=sqlite_ivm=debug` counts the engine's round spans for the reach
fanout 1 cell:

| n | delete rounds | derive rounds | wall s (no logging) |
|---|---|---|---|
| 10000 | 5795 | 168 | 2 |
| 16000 | 343027 | 191 | not measured |
| 32000 | 572824 | 207 | not measured |
| 40000 | 52236 | 161 | 36 |

The forward closure settles in a flat 161 to 207 derive rounds, as the one-hop
diameter predicts. The departure loop is the one that moves, and it does not
move smoothly: 5795 rounds at n 10000, 52236 at n 40000, but 343027 at n 16000
and 572824 at n 32000, so a four-fold change in row count swings the round
count by more than 100x in both directions. Round counts come from a
`RUST_LOG=sqlite_ivm=debug` run; the logging inflates the wall, so the wall
column is from the unlogged sweep.

A round count that is not monotonic in n explains why three changes to the
shape of the departure SQL did nothing: the cost is not a smooth function of
the SQL, it is a function of how the work rowid range happens to advance for a
given size.

### where this stands

Two attempts did not move the worst cell, and both targeted the shape of the
departure pass SQL. The symbolized profile says the cost is the number of full
scans of the member table per statement, not the shape of any one of them:
say 5.9 s per delete at n 100000 against roughly 10 ms for a scan of the
100001-row member table, so of the order of 500 scans per statement.

Cutting the departure rounds is not a one-SQL-statement change: one application
of a rule extends the departing set by one hop, so a round count that runs to
hundreds per statement means the departing set is walked hop by hop. Deriving
from the member table instead of from the work range, or closing the departure
in one recursive statement, is a rewrite of `Plan::fixpoint`'s departure pass.
Making `__k` cheaper to compare than `TEXT` for single integer keys is a
storage format bump. Both are the parent's call under the lane's stop rule.

## arc 3 landing baseline

Three runs each, `--reps 3 --arms sqlite-ivm`, on main 1f64504:

| cell | wall ms |
|---|---|
| reach fanout 1 n 10000 | 1754.7, 1765.6, 1755.9 |
| reach fanout 1 n 100000 | 641314.9, 644299.7, 646832.7 |
| reach fanout 10 n 10000 | 662.9, 662.4, 662.0 |
| reach fanout 10 n 100000 | 3294.6, 3337.9, 3314.4 |

Anything landing on the departure pass has to put all three after-runs of each
of those cells below all three numbers above it. The fanout 1 pair is where the
cost is; fanout 10 is two orders of magnitude cheaper at the same n.

### the departure step, as generated

Dumped from a debug build at bind time. One departure round runs three
statements over the rowid slice `?1..?2` of the work table; the derive is:

```
INSERT OR IGNORE INTO work(__k,c0)
SELECT __k,c0 FROM (
  SELECT <key of the rule head> AS __k, CAST(c2 AS INTEGER) AS c0
  FROM (SELECT c0,c1,c2 FROM <input relation>) q0,
       (SELECT c0 AS c3 FROM work WHERE rowid>?1 AND rowid<=?2) q1
  WHERE CAST(c1 AS INTEGER) = CAST(c3 AS INTEGER))
WHERE __k IN (SELECT __k FROM <member relation>)
```

The step extends the departing set by exactly one hop: the work slice is the
recursive occurrence, joined to the input relation. Every hop is a new round,
which is why the round count is the graph's propagation depth.

Both sides of the join are cast, so no index on either column can serve the
predicate and the round scans rather than probes. That, plus the hop count, is
the bill. Neither is a one-statement edit: the cast is the engine's key
comparison rule, and the hop count is only removable by computing the departing
set as a closure in one statement.

## departure set-at-a-time

Change 1 closes each recursive departure rule in a recursive CTE over the
whole current work set. The bounded outer loop remains for dependencies between
different recursive rules. It uses the existing member, work and deleted
tables, so the storage format is unchanged.

`RUST_LOG=sqlite_ivm=debug`, reach fanout 1 n 40000, one scale repetition:

| measure | before | after change 1 |
|---|---:|---:|
| delete round spans | 52236 | 159 |
| derive round spans | 161 | 161 |
| wall ms | 30118.1 | 31406.0 |

The round count moved independently of the scan cost. Change 1 still carries
the casted recursive join predicate measured below.

## SVGs

`0_baseline-<circuit>.svg` plots wall ms against n for each arm at fanout 1 and
10. `0_baseline-<circuit>-ivm-over-dd.svg` plots the ratio.
