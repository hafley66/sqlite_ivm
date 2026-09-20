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

## step4-identity: `__r` INTEGER, the step that was owed the payback

Six runs, medians. It did not pay. Read this against step 3, not the baseline,
because `__r` is the only thing that changed between them.

| shape | seconds | vs step 3 | db bytes | vs step 3 | vs baseline time | vs baseline bytes |
|---|---|---|---|---|---|---|
| group | 0.352 | worse | 851 968 | worse | 0.93x | 0.97x |
| distinct | 0.213 | flat | 1 126 400 | better | 0.88x | 0.91x |
| join | 0.196 | worse | 1 306 624 | worse | 0.85x | 0.98x |
| fixpoint | 2.380 | better | 26 152 960 | better | 0.96x | 0.99x |

`group` is the clean read: step 3 was 0.333 s and 761 856 bytes, step 4 is
0.352 s and 851 968 bytes. Interning `__r` cost 19 ms and 90 KB per 4000 rows
and bought nothing back. Not one shape beat the TEXT baseline on the clock.

### Why this was structural, not an implementation slip

Interning pays when a composite repeats, and costs when it does not.

| column | distinct composites per 4000 rows | dictionary rows added |
|---|---|---|
| `__k` on Group | 97, one per group | 97 |
| `__k` on Join | 97, one per join key | 97 |
| `__r`, the row identity | 4000, unique by construction | 4000 |

`__r` is one identity per row by definition, so its dictionary has exactly as
many rows as the arrangement. Before this step the composite lived in two
b-tree entries: the `__r` column and its UNIQUE index. After it, the composite
still lives in two entries, in the dictionary table and the dictionary's UNIQUE
index, and the id adds two more in the arrangement and its UNIQUE index. Four
entries where there were two, for a key that can never be shared.

The same argument reaches the fixpoint member table, whose `__k TEXT NOT NULL
UNIQUE` is also the whole row and also unique per row. Step 5 is predicted to
lose for the same reason and for the same arithmetic.

The card's acceptance asks for `__k` and `__r` INTEGER on every arrangement.
The measurement says the first half is right and the second half is not.
