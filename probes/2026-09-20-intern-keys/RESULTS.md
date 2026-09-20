# Interning arrangement keys: write rate per step

`bash probes/2026-09-20-intern-keys/measure.sh <label>`, 4000 source rows per
shape, brew sqlite3 3.51.0, debug `cdylib`, M2 Pro, file-backed db. Median of
three runs; the three agreed to within 3 percent on every cell. Absolute rates
are a debug build and only the ratios between labels are a claim.

Shapes, one per arrangement key kind:

| shape | key shape exercised |
|---|---|
| group | `__k` from the GROUP BY keys, `__r` from the full row |
| distinct | `__k` equal to the whole row, so `__k` and `__r` are the widest |
| join | `__k` on both sides, matched every row |
| fixpoint | member table `__k TEXT NOT NULL UNIQUE`, no `__r` |

## text-baseline (`8150541`, before any edit)

| shape | seconds | db bytes | rows/s |
|---|---|---|---|
| group | 0.328 | 876 544 | 12 212 |
| distinct | 0.188 | 1 241 088 | 21 300 |
| join | 0.167 | 1 335 296 | 23 896 |
| fixpoint | 2.277 | 26 378 240 | 1 757 |
