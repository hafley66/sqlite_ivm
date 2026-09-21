# toward dd: baseline

Yardstick for this lane: per circuit, per n, per fanout, per arm, wall ms,
peak RSS, disk bytes written and read, db bytes on disk, arrangement rows, and
the derived `ivm/dd` (sqlite-ivm wall over dd wall). The job is to push
`ivm/dd` down, circuit by circuit.

Command (13.6 min, one bench process, `nice -n 10`):

```
bench scale --circuits chain,join,group,distinct,window,reach \
  --n 1000,10000,100000 --fanout 1,10 --out plans/costs/toward-dd/0_baseline
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
| chain | 1 | 1000 | 80.5 | 17.2 | 2.9 | 27.941 |
| chain | 1 | 10000 | 345.2 | 30.4 | 3.5 | 97.809 |
| chain | 1 | 100000 | 3247.9 | 166.7 | 6.1 | 530.779 |
| chain | 10 | 1000 | 2132.1 | 551.8 | 63.7 | 33.464 |
| chain | 10 | 10000 | 12932.2 | 1107.3 | 121.5 | 106.415 |
| chain | 10 | 100000 | 29166.7 | 3974.1 | 476.4 | 61.223 |
| distinct | 1 | 1000 | 27.7 | 6.8 | 1.8 | 15.440 |
| distinct | 1 | 10000 | 170.5 | 9.1 | 2.5 | 67.500 |
| distinct | 1 | 100000 | 1408.6 | 15.0 | 4.9 | 285.505 |
| distinct | 10 | 1000 | 26.0 | 6.9 | 1.7 | 15.506 |
| distinct | 10 | 10000 | 30.2 | 7.5 | 1.8 | 17.256 |
| distinct | 10 | 100000 | 170.6 | 11.0 | 1.8 | 96.742 |
| group | 1 | 1000 | 39.6 | 9.7 | 1.7 | 23.068 |
| group | 1 | 10000 | 48.0 | 15.1 | 2.8 | 16.986 |
| group | 1 | 100000 | 54.6 | 39.3 | 6.3 | 8.618 |
| group | 10 | 1000 | 39.1 | 9.9 | 1.7 | 23.243 |
| group | 10 | 10000 | 42.0 | 11.5 | 1.6 | 26.821 |
| group | 10 | 100000 | 45.3 | 28.4 | 1.9 | 23.568 |
| join | 1 | 1000 | 103.0 | 44.8 | 7.5 | 13.806 |
| join | 1 | 10000 | 180.1 | 15.2 | 2.6 | 70.479 |
| join | 1 | 100000 | 1473.3 | 38.2 | 4.9 | 300.633 |
| join | 10 | 1000 | 144.0 | 60.5 | 8.6 | 16.724 |
| join | 10 | 10000 | 4344.5 | 727.6 | 86.9 | 50.001 |
| join | 10 | 100000 | 1989.0 | 326.9 | 42.4 | 46.878 |
| reach | 1 | 1000 | 505.7 | 12.5 | 5.7 | 88.690 |
| reach | 1 | 10000 | 1776.9 | 28.9 | 5.5 | 324.922 |
| reach | 1 | 100000 | 643749.5 | 88.2 | 7.8 | 82207.447 |
| reach | 10 | 1000 | 679.4 | 11.5 | 4.7 | 144.056 |
| reach | 10 | 10000 | 655.7 | 13.9 | 5.1 | 129.797 |
| reach | 10 | 100000 | 3387.5 | 45.6 | 4.9 | 691.568 |
| window | 1 | 1000 | 54.2 | 22.0 | 3.7 | 14.483 |
| window | 1 | 10000 | 184.0 | 26.3 | 3.1 | 59.074 |
| window | 1 | 100000 | 1461.2 | 76.0 | 6.6 | 220.586 |
| window | 10 | 1000 | 60.2 | 18.9 | 4.9 | 12.383 |
| window | 10 | 10000 | 296.5 | 25.0 | 6.4 | 46.156 |
| window | 10 | 100000 | 1510.2 | 75.7 | 7.5 | 201.096 |

## resources

Peak RSS is the getrusage high-water growth across the cell, so only the cell
that first raises the process high-water reports a number. Disk read and write
are the `proc_pid_rusage` counters across the cell. `dd` holds no database, so
its db bytes and arrangement rows are 0.

| circuit | fanout | n | ivm peak RSS MiB | ivm disk read B | ivm disk write B | ivm db B | ivm arr rows | dd peak RSS MiB |
|---|---|---|---|---|---|---|---|---|
| chain | 1 | 1000 | 4.5 | 1114112 | 7168000 | 598016 | 18 | 2.7 |
| chain | 1 | 10000 | 13.6 | 77824 | 26468352 | 4354048 | 18 | 4.3 |
| chain | 1 | 100000 | 105.5 | 405504 | 218419200 | 46985216 | 18 | 57.8 |
| chain | 10 | 1000 | 0.0 | 159744 | 58044416 | 4861952 | 18 | 0.0 |
| chain | 10 | 10000 | 221.1 | 19988480 | 657498112 | 53088256 | 18 | 86.5 |
| chain | 10 | 100000 | 1191.1 | 1918324736 | 2463920128 | 590868480 | 18 | 212.5 |
| distinct | 1 | 1000 | 0.0 | 0 | 4366336 | 249856 | 6 | 0.0 |
| distinct | 1 | 10000 | 0.0 | 4096 | 10436608 | 2322432 | 6 | 0.0 |
| distinct | 1 | 100000 | 0.0 | 40960 | 85438464 | 24764416 | 6 | 0.0 |
| distinct | 10 | 1000 | 0.0 | 0 | 3325952 | 237568 | 6 | 0.0 |
| distinct | 10 | 10000 | 0.0 | 0 | 6901760 | 1171456 | 6 | 0.0 |
| distinct | 10 | 100000 | 0.0 | 32768 | 50941952 | 13393920 | 6 | 0.0 |
| group | 1 | 1000 | 0.0 | 0 | 3289088 | 225280 | 6 | 0.0 |
| group | 1 | 10000 | 0.0 | 8192 | 5668864 | 1282048 | 6 | 0.0 |
| group | 1 | 100000 | 0.0 | 32768 | 38322176 | 13549568 | 6 | 0.0 |
| group | 10 | 1000 | 0.0 | 16384 | 3252224 | 212992 | 6 | 0.0 |
| group | 10 | 10000 | 0.0 | 4096 | 5365760 | 1146880 | 6 | 0.0 |
| group | 10 | 100000 | 0.0 | 8192 | 35909632 | 11448320 | 6 | 0.0 |
| join | 1 | 1000 | 0.0 | 0 | 9097216 | 733184 | 11 | 0.0 |
| join | 1 | 10000 | 0.0 | 0 | 17772544 | 3178496 | 11 | 0.0 |
| join | 1 | 100000 | 0.0 | 20480 | 130355200 | 34549760 | 11 | 0.0 |
| join | 10 | 1000 | 0.0 | 16384 | 8466432 | 782336 | 11 | 0.0 |
| join | 10 | 10000 | 0.0 | 45056 | 295567360 | 36831232 | 11 | 0.0 |
| join | 10 | 100000 | 0.0 | 49152 | 392556544 | 80379904 | 11 | 0.0 |
| reach | 1 | 1000 | 0.0 | 16384 | 10252288 | 417792 | 13 | 0.0 |
| reach | 1 | 10000 | 0.0 | 0 | 52850688 | 4161536 | 13 | 0.0 |
| reach | 1 | 100000 | 0.0 | 125796352 | 801234944 | 45768704 | 13 | 0.0 |
| reach | 10 | 1000 | 0.0 | 40960 | 9097216 | 405504 | 13 | 0.0 |
| reach | 10 | 10000 | 0.0 | 77824 | 16777216 | 2363392 | 13 | 0.0 |
| reach | 10 | 100000 | 0.0 | 217088 | 154308608 | 25915392 | 13 | 0.0 |
| window | 1 | 1000 | 0.0 | 0 | 7999488 | 368640 | 6 | 0.0 |
| window | 1 | 10000 | 0.0 | 0 | 13107200 | 2551808 | 6 | 0.0 |
| window | 1 | 100000 | 0.0 | 16384 | 102457344 | 27525120 | 6 | 0.0 |
| window | 10 | 1000 | 0.0 | 4096 | 7532544 | 356352 | 6 | 0.0 |
| window | 10 | 10000 | 0.0 | 57344 | 34381824 | 2289664 | 6 | 0.0 |
| window | 10 | 100000 | 0.0 | 16384 | 96595968 | 22888448 | 6 | 0.0 |

## the three worst cells

1. `reach` fanout 1 n 100000: `ivm/dd` 82207.447. sqlite-ivm 643749.5 ms
   against dd 7.8 ms. Inside the cell: delete 5919.2 ms per statement, update
   7691.3 ms, replace 98947.3 ms.
2. `reach` fanout 10 n 100000: `ivm/dd` 691.568. sqlite-ivm 3387.5 ms against
   dd 4.9 ms.
3. `chain` fanout 1 n 100000: `ivm/dd` 530.779. sqlite-ivm 3247.9 ms against
   dd 6.1 ms.

Every cell is above 3x: the best is `group` fanout 1 n 100000 at 8.618, the
worst 82207.447. The stop condition is far off.

## defects

Cell columns over 10 s, printed by the sweep rather than hidden:

| circuit | fanout | n | arm | column | ms |
|---|---|---|---|---|---|
| chain | 10 | 10000 | sqlite-ivm | wall | 12932.2 |
| chain | 10 | 100000 | sqlite-ivm | wall | 29166.7 |
| reach | 1 | 100000 | sqlite-ivm | wall | 643749.5 |
| reach | 1 | 100000 | sqlite-ivm | replace | 98947.3 |

The `reach` fanout 1 n 100000 cell is the single worst: its deletes and updates
are three orders of magnitude above every other n, and its disk write (801
MB) and disk read (126 MB) are the largest in the sweep. That cell alone is
10.7 min of the 13.6 min run.

## SVGs

`0_baseline-<circuit>.svg` plots wall ms against n for each arm at fanout 1 and
10. `0_baseline-<circuit>-ivm-over-dd.svg` plots the ratio.
