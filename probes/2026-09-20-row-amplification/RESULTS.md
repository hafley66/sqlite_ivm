# Copies of one source row across sqlite_ivm state

Measured 2026-09-20 with `measure.py`. Circuit, rows and probe row:

```sql
CREATE TABLE a(id INTEGER PRIMARY KEY, k INTEGER, v INTEGER);
CREATE TABLE b(id INTEGER PRIMARY KEY, k INTEGER, w INTEGER);
SELECT sqlite_ivm_create('chain',
  'SELECT a.k AS k, SUM(b.w) AS total FROM a JOIN b ON a.k=b.k WHERE a.v>0 GROUP BY a.k');
-- 2000 rows each; k = id % (2000/fanout); a.v = 1000000+id; b.w = 2000000+id
-- probe row: a.id=1, v=1000001. No other cell in a or b holds 1000001.
```

A copy is a stored cell whose integer value is 1000001, or whose text contains `1000001`.
Every table in the file is scanned, every column.

## Two binaries

| label | commit | arrangement columns | path |
|---|---|---|---|
| pre-#19 | `b5ad049` | `__k, __r(TEXT), __n, c0..` | `sqlite_ivm/target/release/libsqlite_ivm.dylib` (built 02:07) |
| HEAD | `7551b9e` | `__k, __r(INTEGER hash), __c(TEXT), __n, c0..` | `~/.agent/lanes/review-amplification/target/release/libsqlite_ivm.dylib` |

## Copies at fanout 40

| table | rows | pre-#19 copies | HEAD copies | which cells |
|---|---|---|---|---|
| `a` | 2,000 | 1 | 1 | `v` |
| `chain_op4_0` | 2,000 | 2 | 2 | `c2`, plus `__r` (pre-#19) or `__c` (HEAD) |
| `chain_op4_1` | 2,000 | 0 | 0 | |
| `chain_op6_0` | 80,000 | 80 | 80 | `c2` ×40, plus `__r` ×40 (pre-#19) or `__c` ×40 (HEAD) |
| `chain_state` | 50 | 0 | 0 | SUM hides the value |
| `chain_keys` | 50 | | 0 | HEAD only |
| **total** | | **83** | **83** | |

The finding said 82. Its own table sums to 83. Both binaries store 83.

## Copies track fanout

| fanout | copies pre-#19 | copies HEAD | `chain_op6_0` rows | file bytes pre-#19 | file bytes HEAD |
|---|---|---|---|---|---|
| 1 | 5 | 5 | 2,000 | 913,408 | 876,544 |
| 10 | 23 | 23 | 20,000 | 3,026,944 | 2,498,560 |
| 40 | 83 | 83 | 80,000 | 10,317,824 | 8,413,184 |
| 100 | 203 | 203 | 200,000 | 24,338,432 | 20,258,816 |

Copies = 3 + 2·fanout. One in the source, two in the a-side arrangement, two per
join partner in the group-input arrangement.

## Why two per row

Every arrangement row holds the tuple twice: once as `c0..cN` columns, once serialized
in the row-identity text. HEAD sample from `chain_op6_0`:

```
__k | __r                  | __c                              | __n | c0 | c1 | c2      | c3 | c4 | c5
1   | -9198304574145832690 | [1,1,1000001,1,1,2000001]        | 1   | 1  | 1  | 1000001 | 1  | 1  | 2000001
```

Table DDL: `src/1a_relational.rs:360`. Insert that writes both: `src/1a_relational.rs:560`.
The group operator's input arrangement carries every column of the join output because
the planner pushes `Kind::Group` directly on the join node (`src/0b_relational.rs:1339-1350`)
with no projection between them.

## Bytes at fanout 40 (dbstat, table plus its indexes)

| table | pre-#19 | HEAD |
|---|---|---|
| `chain_op4_0` | 180,224 | 155,648 |
| `chain_op4_1` | 180,224 | 155,648 |
| `chain_op6_0` | 9,826,304 | 7,962,624 |
| `chain_state` | 8,192 | 8,192 |

Raw receipts: `old.jsonl`, `head.jsonl`.

Rerun:

```bash
python3 probes/2026-09-20-row-amplification/measure.py <libsqlite_ivm.dylib> 1 10 40 100
```
