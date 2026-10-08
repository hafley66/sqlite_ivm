# The gang pages DD arrangements into SQLite blobs

Hypothesis: DD 0.25.1's columnar trace plus its spill hook (`columnar::trace::spill`) holds an
arrangement's resident records under a fixed budget, with the paged chunks stored as blobs in a
SQLite table, and peak RSS stays flat as the arrangement grows past the budget.

Arms, one arrangement of `(key, val)` updates that stay live (no cancelling):

| arm | spine | spill store |
|---|---|---|
| `vec` | `ord_neu::ValSpine` (what `ivm-dd` uses) | none |
| `col` | `columnar::trace::Spine` | none |
| `col-file` | columnar | lz4 into a tempfile (DD's example store) |
| `col-sqlite` | columnar | lz4 into a SQLite `chunk(id INTEGER PRIMARY KEY, bytes BLOB)` table |

Measured per arm: peak RSS (`getrusage`), wall time, spilled chunks and records.
