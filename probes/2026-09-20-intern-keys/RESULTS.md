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

## step2-group: `__k` INTEGER on the Group kind only

Only `group` should move. It moved in both directions: 13.1 percent smaller on
disk, 1.6 percent slower on the clock.

| shape | seconds | db bytes | rows/s | vs baseline time | vs baseline bytes |
|---|---|---|---|---|---|
| group | 0.333 | 761 856 | 12 025 | 0.98x | 0.87x |
| distinct | 0.188 | 1 249 280 | 21 307 | 1.00x | 1.01x |
| join | 0.168 | 1 343 488 | 23 769 | 0.99x | 1.01x |
| intern | 0.003 | 430 080 | 1 319 697 | new | new |
| fixpoint | 2.262 | 26 386 432 | 1 768 | 1.01x | 1.00x |

The time regression is outside the run band: three step2 runs spanned
0.333..0.339 and three baseline runs spanned 0.325..0.329, so the two sets do
not overlap.

Cause, and why it is the expected shape rather than a defect: the Group
arrangement still carries `__r TEXT NOT NULL UNIQUE`, which is the fatter of
its two indexes. This step narrowed the `__k` index and added one dictionary
seek per maintained row, and removed nothing. The 13.1 percent on disk is the
`__k` index and the `__k` column shrinking to 8 bytes; the 1.6 percent on the
clock is that one added seek. The time win is priced against `__r`, which is
step 4, so this number is only readable as a pair with that one.

## step3-all-kinds: `__k` INTEGER on Set, Join and the Fixpoint inputs

Six runs, not three: the run band widened enough that three would not separate
the shapes. Medians below.

| shape | seconds | db bytes | rows/s | vs baseline time | vs baseline bytes |
|---|---|---|---|---|---|
| group | 0.333 | 761 856 | 12 017 | 0.98x | 0.87x |
| distinct | 0.211 | 1 335 296 | 19 183 | 0.89x | 1.08x |
| join | 0.178 | 1 216 512 | 22 490 | 0.94x | 0.91x |
| intern | 0.003 | 430 080 | 1 287 830 | new | new |
| fixpoint | 2.495 | 26 554 368 | 1 548 | 0.91x | 1.01x |

Every shape is slower and `distinct` is now fatter on disk than the TEXT
baseline. The cause is the same one step 2 named, and `distinct` shows it
clearest: its `__k` is the whole row, so the dictionary stores a second copy
of exactly what `__r TEXT NOT NULL UNIQUE` already stores, and neither copy
has gone away yet. `join` and `group` shrink because their `__k` is a proper
subset of the row, so the id is smaller than the composite it replaced.

Both remaining numbers are owed to step 4. If removing `__r` does not pay back
the dictionary, the card's premise is wrong and the answer is to revert, not to
keep going.

A correctness note that is not about speed: bulk join maintenance tested for a
NULL join component with `json_each(__k)`. An id is not parseable JSON, so that
predicate now reads the composite back out of the dictionary.

## step4-dictionary: `__r` interned, reverted

Recorded here because the tree no longer carries it. Six runs, medians, against
step 3: group 0.352 s and 851 968 bytes against 0.333 s and 761 856; join
0.196 s and 1 306 624 bytes against 0.178 s and 1 216 512. Worse on both axes,
every shape slower than the TEXT baseline.

Interning pays when a composite repeats and costs when it does not.

| column | distinct composites per 4000 rows |
|---|---|
| `__k` on Group | 97, one per group |
| `__k` on Join | 97, one per join key |
| `__r`, the row identity | 4000, unique by construction |

`__r` is one identity per row by definition, so its dictionary has exactly as
many rows as the arrangement. The composite used to sit in two b-tree entries,
the column and its UNIQUE index; interning moves those two into the dictionary
and adds two more for the id. Four where there were two, for a key that can
never be shared. Reverted in `88f0b0b`.

## step4-hash: `__r` is an FNV-1a 64 of the composite, tiebreak recomputes it

`__r INTEGER NOT NULL` with a plain index, no UNIQUE. Every lookup filters on
the hash then confirms byte-exactly against the composite rebuilt from the
stored `c{i}` columns. Six runs, medians.

| shape | seconds | db bytes | rows/s | vs baseline time | vs baseline bytes |
|---|---|---|---|---|---|
| group | 0.411 | 520 192 | 9 691 | 0.80x | 0.59x |
| distinct | 0.256 | 1 167 360 | 15 630 | 0.73x | 0.94x |
| join | 0.250 | 966 656 | 15 966 | 0.67x | 0.72x |
| intern | 0.003 | 434 176 | 1 222 120 | new | new |
| fixpoint | 2.600 | 26 247 168 | 1 538 | 0.88x | 1.00x |

The space claim lands: `group` is 41 percent smaller than the TEXT baseline and
`join` is 28 percent smaller, which is JSON text leaving two indexes per
arrangement. No index anywhere now holds a variable-length key.

The clock went the other way, further than any earlier step. The cause is the
tiebreak, not the hash. `change()` issues three statements per maintained row
and each one rebuilds `json_array(CASE typeof(c0) ... )` across every column of
the candidate row before comparing. That is an N-column expression with a
`typeof` branch and a `sqlite_ivm_real_hex` call per column, evaluated where the
old code compared one stored string.

The fix is to stop recomputing what the row could store: keep the composite as
an unindexed `TEXT` column and leave only the hash in the index. That is one
string compare per lookup instead of an N-column rebuild, and the index stays
integer. Measured next as step 4b.
