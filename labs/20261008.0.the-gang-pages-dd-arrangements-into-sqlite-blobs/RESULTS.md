# Results

macOS aarch64 (M-series), release build cross-compiled on spark-2 by rcargo, 1 worker, 64 epochs,
spill budget 250,000 records. Raw lines: `results2.tsv` (arm, sizes, peak RSS, wall, spill counts,
retained records). `retained_records` equals `keys` in every run: the trace holds every row.
`results.tsv` is an earlier run whose traces were dropped inside the dataflow; ignore it.

| arm | row shape | 2M RSS MB | 8M RSS MB | 8M wall s |
|---|---|---|---|---|
| vec | `(u64,u64)` | 192 | 796 | 1.56 |
| col | `(u64,u64)` | 330 | 2,117 | 4.15 |
| col-file | `(u64,u64)` | 219 | 962 | 4.70 |
| vec-row | `(Vec<i64>[1], Vec<i64>[3])`, ivm-dd's shape | 598 | 2,250 | 4.66 |
| col-row | same | 606 | 3,989 | 6.64 |
| col-row-file | same | 443 | 1,420 | 8.16 |
| col-row-sqlite | same | 499 | 1,503 | 8.61 |

1. Row shape: `Vec<i64>` rows cost 2.8x fixed `(u64,u64)` at 8M keys (vec-row vs vec).
2. Columnar without spill costs more than `ord_neu::ValSpine` here (2.7x fixed, 1.8x rows).
3. Spill lowers peak (col-row-file 37% under vec-row at 8M) but peak still grows 3.2x for 4x keys:
   the 250k-record budget does not bound peak RSS.
4. SQLite blob store vs tempfile store: +6% RSS, +6% wall.

Open: where the non-flat spill peak lives (merge transients that fetch both inputs, chunk
metadata, or the batcher's per-epoch buffer).
