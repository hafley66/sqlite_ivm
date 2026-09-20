# Interning arrangement keys: write rate per step

`bash probes/2026-09-20-intern-keys/measure.sh <label>`, 4000 source rows per
shape, brew sqlite3 3.53.2, debug `cdylib`, M2 Pro, file-backed db. Median of
three runs; the three agreed to within 3 percent on every cell. Absolute rates
are a debug build and only the ratios between labels are a claim.

Shapes, one per arrangement key kind:

| shape | key shape exercised |
|---|---|
| group | `__k` from the GROUP BY keys, `__r` from the full row |
| distinct | `__k` equal to the whole row, so `__k` and `__r` are the widest |
| join | `__k` on both sides, matched every row |
| fixpoint | member table `__k TEXT NOT NULL UNIQUE`, no `__r` |
| intern | the dictionary write alone, every composite distinct |

## text-baseline (`8150541`, before any edit)

| shape | seconds | db bytes | rows/s |
|---|---|---|---|
| group | 0.328 | 876 544 | 12 212 |
| distinct | 0.188 | 1 241 088 | 21 300 |
| join | 0.167 | 1 335 296 | 23 896 |
| fixpoint | 2.277 | 26 378 240 | 1 757 |

The `intern` shape has no baseline row: before the dictionary existed there was
nothing to time.

## step1-dictionary: the dictionary table, `intern`, `resolve`

The engine does not call either operation yet, so the four fold shapes are a
control and must not move. They did not: every cell is inside the 3 percent
run-to-run band, and the db grows by one empty table and its UNIQUE index.

| shape | seconds | db bytes | rows/s | vs baseline |
|---|---|---|---|---|
| group | 0.328 | 888 832 | 12 199 | 1.00x |
| distinct | 0.189 | 1 249 280 | 21 126 | 0.99x |
| join | 0.168 | 1 343 488 | 23 743 | 0.99x |
| fixpoint | 2.338 | 26 386 432 | 1 711 | 0.97x |
| intern | 0.003 | 430 080 | 1 217 656 | new |

The intern cost alone: 0.75 microseconds per distinct composite, against 82
microseconds for one `group` source row through the whole fold. One intern is
about 1 percent of the row it would replace a TEXT key on, so the budget for
the remaining steps is wide. This prices the bulk SQL intern; the incremental
Rust `intern()` pays a second prepared statement on a dictionary miss.
