# Instrumented eight-engine IVM shootout

[20 workload PNGs](charts/index.md) | [Coverage and timing report](report.txt) | [Coverage TSV](coverage.tsv)

Command completed 2026-09-09:

```bash
bash sqlite_ivm/scripts/16_shootout.sh quick --out /private/tmp/ivm-telemetry-final-20260909
```

Eight implementations, 20 workload families, 400 initial rows in source a, one warmup and three measured repetitions. 140 validated performance cells; 20 unsupported pg_ivm cells. SQLite IVM, DD and Prolog passed all 20 circuits and 44 typed cases. DD also passed its 13 contract checks. The two recorded FULL JOIN USING mismatches are in pg_ivm 1.15 and PGlite pg_ivm 1.13; the command exits 1 to retain these failures. No performance execution cells were incomplete.

All 5,460 measured mutation records include state inventories outside timed regions. All 480 measured process records include positive peak RSS. The 2,340 PostgreSQL/PGlite mutation records include database-statistics deltas with units. Process records include supported CPU, page-fault, context-switch, and block-I/O counters. Unsupported sensors retain null values and reasons. RSS includes adapter, validation, and telemetry memory; native PostgreSQL RSS samples its server process group.

[Validation fixtures and logs](validation-logs.tar.gz) retain the semantic inputs, competitor receipts, and command logs.

The data files are gzip-compressed without filtering records. Read with `gzip -dc performance.jsonl.gz`, or decompress beside the archive with `gzip -dk performance.jsonl.gz`. SHA256SUMS records hashes of the original uncompressed data. Plot indexes link to compressed inventory and telemetry exports. Source revision and source fingerprints are in run.json and report.json.gz; this run measured the uncommitted source represented by those fingerprints.

Quick measures one input tier. The separate full profile measures two tiers across its six selected workloads. Latency percentiles here use three samples and do not establish production tail latency.
