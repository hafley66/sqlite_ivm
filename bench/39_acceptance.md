# Historical 0.1.0 native acceptance, 2026-09-09

Current 0.2.0 feature acceptance is recorded in [46_feature_acceptance.md](46_feature_acceptance.md). These timings belong to the older binary below.

The tested and packaged extension has SHA-256 `c78ae3f2b761cb0af5fb8f4c0ba89079fc6416b7e26aa286dd2e8c05886b8e09`.
Both final receipts fingerprint this exact binary and its Rust source files.

- 33 Rust integration tests passed: 6 binding, 13 maintenance, 5 lifecycle, 9 relational.
- Six native Bash scenarios passed.
- All 20 circuit families passed 13 states through the loaded library, followed by reopen, rename, rollback and drop.
- The 400-row shared comparison passed five measured repetitions after one warmup: 4550 exact mutation checks across admitted engines.
- The 12,000-row shared comparison passed one final acceptance repetition: 780 exact mutation checks across SQLite IVM, DD and full-query SQLite.
- SQLite IVM supplied 1,560 measured mutation checks across these two final receipts, with no unsupported families, mismatches, timeouts or consumer failures.

## Performance observations

At 400 rows, SQLite IVM's median whole-case time was 2.47 to 6.25 times lower than pg_ivm across its 10 supported families. DD and full-query SQLite had lower whole-case medians than the plugin in this cell. Whole-case totals include initial bulk loading and clearing all edges; consult TSV state categories before attributing those totals to small updates.

SQLite/PG durability is enabled; DD is volatile. These are local observations on macOS arm64 with 16 GiB RAM. No CPU isolation or enforced memory cap was used. SQLite was 3.53.2 and PostgreSQL was 18.6. The final 12k run is a correctness acceptance run, with one observation per engine/family rather than a stable timing estimate.

A separate earlier 12k PostgreSQL self-join diagnostic exhausted available disk; its cluster was removed. It is excluded from this table and cannot establish a completed 12k PG comparison. Final 400-row PostgreSQL runs used a 128 MB temporary-file limit. PostgreSQL is not an arm in the final 12k receipt.

| Family | SQLite IVM 400 median ms | pg_ivm 400 median ms | DD 400 median ms | SQLite query 400 median ms | SQLite IVM 12k observed ms |
|---|---:|---:|---:|---:|---:|
| pipeline | 12.84 | 74.42 | 0.46 | 3.33 | 595.81 |
| fanout_fanin | 15.84 | unsupported | 0.54 | 3.37 | 650.91 |
| distinct | 19.27 | 103.06 | 0.73 | 3.46 | 1212.03 |
| join | 15.74 | 79.56 | 0.54 | 3.06 | 580.28 |
| self_join | 86.67 | 270.92 | 1.05 | 5.33 | 2952.15 |
| chain | 32.79 | 121.12 | 0.82 | 3.73 | 1159.31 |
| diamond | 47.05 | unsupported | 0.85 | 5.44 | 1920.93 |
| semijoin | 19.96 | 81.95 | 0.65 | 3.10 | 764.83 |
| antijoin | 30.10 | unsupported | 0.85 | 3.51 | 1453.36 |
| aggregate_churn | 36.09 | 88.97 | 0.59 | 3.15 | 1172.95 |
| reach_cycle | 20.59 | unsupported | 1.34 | 3.31 | 810.71 |
| minmax | 32.55 | 136.25 | 0.58 | 3.88 | 1291.49 |
| count_distinct | 29.20 | unsupported | 0.67 | 3.51 | 1171.00 |
| union_set | 21.94 | unsupported | 0.76 | 3.48 | 1279.20 |
| except_set | 22.31 | unsupported | 0.98 | 3.79 | 1343.09 |
| intersect_set | 18.52 | unsupported | 0.79 | 3.13 | 878.63 |
| topk | 30.82 | unsupported | 0.69 | 3.17 | 1509.44 |
| window_rank | 58.23 | unsupported | 0.95 | 4.65 | 2364.80 |
| subquery | 13.08 | 79.71 | 0.45 | 3.15 | 574.63 |
| cte | 12.97 | 81.14 | 0.45 | 3.25 | 568.16 |

## Receipts and scope

- [400-row receipt](results/acceptance-400.jsonl), SHA-256 `fbe4696417baa8d5602455968130afef50f10821175cc12537c23025ca7c75f8`.
- [12k receipt](results/acceptance-12k.jsonl), SHA-256 `ba2dd60113f9f00ad629fd0941a0ee86c1dd1b7a38a9dc08d0e617f3f9db43f8`.
- [400-row TSV](results/acceptance-400.tsv) and [12k TSV](results/acceptance-12k.tsv).
- [Reproduction and timing contract](README.md).

Generated receipts and databases remain local and are excluded from source archives. The hashes above identify the retained results; reruns produce fresh receipts. The source archive includes the fixed generators, independent oracle and adapters.

Coverage means these 20 bounded SQL circuit families, not every SQL combination or arbitrary DD operators/time domains. Managed source DDL covers table/column rename and restrict/cascade drop. Direct source ALTER/DROP and ADD/DROP COLUMN remain outside the protocol. Linux/macOS CI is wired; remote CI execution and external publication have not occurred in this local task.
