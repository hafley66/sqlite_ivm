# Index and maintenance audit, 2026-09-22

[Visual comparison](https://sqlite-ivm-index-report.hafley66.chatgpt.site)
shows installation DDL, fact changes, dimension changes and the observed plans.
[Samples and query plans](6_index_audit.json) retain the measured evidence.

## Existing join indexes

```text
Current dimension delta
    │
    └─ intern group key ─► summary_op4x0(__k) B-tree ─► matching fact images

Historical dimension delta
    │
    └─ group_id ─────────► fact(group_id) B-tree ─────► matching source facts

DD dimension delta
    │
    └─ group key ────────► arranged in-memory batches ─► matching fact values
```

The current plan reports `SEARCH l USING INDEX __ivm_summary_op4_0_key
(__k=?)`. The historical plan reports `SEARCH fact USING INDEX
native_result_live_key_0 (group_id=?)`. Fact changes probe the opposite input:
the current dimension arrangement index or the historical dimension integer
primary key. Both SQLite arms use real indexes.

DD's fixture calls `join`, whose implementation arranges both inputs by key.
Its retained batches contain keys, values, logical times and signed weights.
SQLite provides indexed lookup with B-trees and cached pages; the SQLite arms
also persist changes through WAL/FULL. The DD arm provides no durable commit.

## DDL and DML sequences

The old counted lab is already a loadable SQLite virtual-table extension.
Both arms receive the same source mutations and use a batch flush at the
transaction boundary. Their generated maintenance differs:

| Stage | Current | Historical counted source-view arm |
|---|---|---|
| Install | Create operator arrangements, indexes, result, metadata, source hooks | Create delta/result tables, attach sources, install source views and source-key indexes |
| Join | Delta left against old right; update left; new left against delta right | Delta left against new right + new left against delta right − delta left against delta right |
| Retained inputs | Update join inputs and group members | Probe indexed physical sources; copied state is emptied during setup |
| Aggregate/result | Update nullable support, calculate delta, retract/insert bag output | Grouped INSERT SELECT with direct count/sum/non-null accumulator UPSERT |
| Finish | Sweep scratch, commit source + state | Validate support, drop empty groups, clear delta, commit source + state |

Persistent schema is installed at creation. Scratch DDL runs at bind/rebind,
outside ordinary drains. Drains execute DML. `CREATE TEMP TABLE measured_output`
in the trace belongs to the benchmark's timed result materialization.

The current state model has transaction/lifecycle coverage, including rollback,
WAL readers, rename, reopen, ownership validation and defensive shadow protection.
That evidence does not establish minimum storage work. Convergence with the old
lab requires the source-indexed join/aggregate sequence for eligible compiled
plans, preserving the cross-term rule when both sources change, plus duplicate,
NULL, overflow, rollback, savepoint, reopen and managed-DDL behavior.

## Temporary before-image index

The aggregate delta query previously reported `SCAN b LEFT-JOIN`. Each changed
group scanned the temporary before-image. The patch creates an index on the
projected key columns and removes the computed integer lookup expression's
affinity with unary `+`, allowing SQLite to search the untyped scratch index.
Without that affinity adjustment the new index still was not selected.

```text
before: changed groups ─► SCAN b LEFT-JOIN
after:  changed groups ─► SEARCH b USING INDEX ..._group_c0 (c0=?) LEFT-JOIN
```

Index names include the projected columns because different views can share a
scratch table while projecting their keys into different positions. The growth
test now creates both layouts and asserts that both actual aggregate queries
use indexed searches. The existing constant-work assertion remains in place.

Five alternating executions of the same 12,000/1,000/200 fixture measured
**73.235 ms baseline / 72.942 ms indexed**, with overlapping sample ranges.
This does not establish a wall-time improvement. All five states matched for
each run. A separate nine-case, three-arm replay also matched all hashes;
its target case was 73.172 ms indexed / 21.218 ms historical / 1.031 ms DD.
The historical gap remains about 3.4× on that run.

The paired receipt captures the first index name before adding the projected
column suffix; the suffix prevents cross-view index-name collisions and does
not change the single-view benchmark's index columns or aggregate SQL.
