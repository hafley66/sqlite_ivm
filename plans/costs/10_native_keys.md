# Native join/group keys and result row IDs

Local visual report: [8_index_audit.html](8_index_audit.html). Machine-readable
receipts and executed SQL plans: [9_native_keys.json](9_native_keys.json).

## Physical changes

- Persistent copies of operator inputs are removed; compiled source reads and source indexes supply current rows.
- Join/group keys occupy native SQLite columns. Composite indexes cover those columns. `=` implements join equality and `IS` matches NULL group keys.
- Changed groups drive indexed result-row lookups. Results have explicit `INTEGER PRIMARY KEY AUTOINCREMENT` IDs. Projected aggregate groups update in place.
- Result identity and delta consolidation compare native values, storage types and exact REAL bits. Whole-row JSON identities are absent from join/group input reads and all result writes.
- An integer checksum supports corruption recovery and is never used as a key. Set and recursive membership retain encoded keys.
- Storage format 8 migrates compatible older layouts and preserves public result declarations. Failed-DDL reconnects share their pending batch; recursive deletion reads the old edge relation.

## Same workload, measured results

Target: 12,000 fact rows, batch 1,000, dimension fanout 200. Each process checks
initial, insert, update, delete and dimension-fanout states against the same
fixture hash `03a093bf232f8f34a9f254c0ef128ed5c7b9a2fd220b3164ab0b12d6a74d75e7`.

Five alternating runs against the archived copied-input binary: **79.171 ms →
27.019 ms**, **2.93× faster**. The archive is identified by its binary hash in
the receipt. This comparison includes the input-copy removal and native-key/row-ID changes.

Separate rotating three-arm comparison, three repetitions: current
**26.020 ms**, historical C lab **21.228 ms**,
DD **1.137 ms**. Current/historical: **1.23×**.
Timings sum mutation/maintenance plus materialized result reads. Setup and oracle
checks are outside timing. Historical SQLite configures an 8 MiB cache; current
uses its default. SQLite is durable; DD is volatile. All reported state hashes match.

| Rows | Batch | Fanout | Current ms | Historical ms | DD ms |
|---:|---:|---:|---:|---:|---:|
| 400 | 10 | 10 | 3.006 | 1.683 | 0.138 |
| 12000 | 10 | 10 | 3.701 | 2.663 | 0.233 |
| 400 | 10 | 200 | 3.148 | 1.942 | 0.150 |
| 1200 | 10 | 200 | 3.942 | 2.160 | 0.166 |
| 4000 | 10 | 200 | 3.854 | 2.336 | 0.190 |
| 12000 | 10 | 200 | 4.245 | 2.695 | 0.244 |
| 12000 | 1000 | 200 | 26.020 | 21.228 | 1.137 |
| 12000 | 1 | 200 | 3.706 | 2.405 | 0.270 |
| 12000 | 100 | 200 | 10.520 | 7.154 | 0.356 |

## Historical implementation

Recovered from `sprefa@e2052d5ae`,
`v6/labs/exec_shootout/postgres_pglite_ivm/43_sqlite_competitive/0c_batch.h`.
`source_view_setup` creates indexes on source key columns. `flush_batch` joins
those native columns, groups signed contributions, and directly adds COUNT/SUM
support into results with UPSERT. Its delta and result tables use JSON tuples as
single conflict keys. A JSON array containing NULL is a non-NULL key, so repeated
NULL-containing tuples conflict in the same row.

## Delta algebra and remaining recomputation

```text
current sources + signed changes
        |
        +--> inner join: dL*newR + newL*dR - dL*dR
        +--> eligible integer COUNT/SUM: add signed contributions
        +--> other affected keys: evaluate before/after, subtract bags
```

For one inserted row on each side of a key with one existing row on each side:
COUNT contributions are `2 + 2 - 1 = 3`; old count 1 becomes 4. The cross-term
would otherwise occur twice. This is a custom SQL delta engine, with transaction
and read drains as batch boundaries. It has no DBSP runtime or timestamp-frontier
API. The affected-key paths still recompute their scoped result.

## Verification and commands

75 root tests pass, including native query-plan assertions, mixed-type composite
keys, duplicate bags, both-input mutations, rollback, stable group row IDs through
VACUUM, checksum collisions, migration and DDL. Native loading, CRUD/rollback
scripts, and the CLI gate pass. Shared collector rename/rollback tests pass in
hafley-rs at `29ce8a5e`; application SQL remains here.

```bash
cd /Users/chrishafley/projects/sqlite_ivm && NEXTEST_TEST_THREADS=2 just verify
cd /Users/chrishafley/projects/sqlite_ivm && just crossover-observe
open /Users/chrishafley/projects/sqlite_ivm/plans/costs/8_index_audit.html
```

Timing receipts preserve the measured binary and source-patch hashes. The final
source also removes the unused JSON-generating join/group branches; the executed
SQL is unchanged by that cleanup.
