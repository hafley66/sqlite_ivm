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

## profile, `reach` fanout 1 n 100000

```
samply record --save-only -o /tmp/reach-f1-n100k.json -- \
  bench scale --circuits reach --n 100000 --fanout 1 --arms sqlite-ivm
```

663488 samples at 1 kHz, 664.4 s. Leaf self time from the profile JSON, frames
symbolized with `atos` against the bench binary.

Inclusive: `<sqlite_ivm::vtab::Table>::drain` 661.7 s (99.6%),
`<sqlite_ivm::relational::Plan>::drain` 648.9 s (97.7%),
`CachedExecute::execute_cached` 652.2 s (98.2%). The cell is one drain per
statement.

Self, grouped by symbol:

| frame | s | share |
|---|---|---|
| `sqlite3VdbeExec` | 353.6 | 53.2% |
| `sqlite3KeyInfoUnref` | 85.8 | 12.9% |
| `sqlite3VdbeFreeCursorNN` | 61.0 | 9.2% |
| `memjrnlWrite` | 44.6 | 6.7% |
| system frame at 0x2704 (malloc or kernel; `atos` cannot split the two) | 37.3 | 5.6% |
| `sqlite3PagerSharedLock` | 20.5 | 3.1% |
| `sqlite3VdbeHalt` | 11.8 | 1.8% |
| `btreeInvokeBusyHandler` | 9.7 | 1.5% |
| `newDatabase` | 7.8 | 1.2% |

Split by call site inside `apply`: 559.4 s in the 120 single-statement writes,
101.6 s in the one-transaction 1000-row replace.

Hypothesis: the fixpoint walks one hop per round and each round builds its work
set over ephemeral b-trees, so statement and ephemeral-btree setup and teardown
is paid per round. `sqlite3KeyInfoUnref`, `sqlite3VdbeFreeCursorNN`,
`memjrnlWrite`, `sqlite3PagerSharedLock`, and `newDatabase` together are 33.1%
of the cell, 220 s, against 53.2% of real VM work. The number to move is that
220 s: a round that reuses its cursors and its scratch b-tree instead of
opening them should take it near zero.

Per-statement cost at fanout 1 backs the round count as the driver: delete is
0.4 ms at n 1000, 10.9 ms at n 10000, 5960.0 ms at n 100000; the 10x step from
10000 to 100000 is 547x.

Blocked. Any change here is in `src/`, and `refactor-pass-3b-one-engine` is
live on `src/`, so this arc waits for that merge and rebases. `bench scale
--reps N` is in place for the three-runs-each-side rule.

## SVGs

`0_baseline-<circuit>.svg` plots wall ms against n for each arm at fanout 1 and
10. `0_baseline-<circuit>-ivm-over-dd.svg` plots the ratio.
