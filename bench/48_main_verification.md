# Native component commit verification, 2026-09-09

The component and standalone DD harness were formatted before commit. Fresh
local execution against SQLite 3.53.2 passed:

- 42 Rust integration tests, six Bash CRUD/lifecycle scenarios, and 20 native circuit families.
- 44 feature cases with 174 states each: 7,656 checks per native/DD arm, including independent source/output comparisons.
- Nine tests against the loaded native library for features, values and transactions.

The native library SHA-256 is
`dd834d231d3b5a7dfd4f59c3e77ec303d3c69db703038f549709a6829dbca9b4`.

[The manifest](receipts/0_main_20260909/manifest.json) records the tested source
and library hashes. [Native](receipts/0_main_20260909/native.jsonl),
[DD](receipts/0_main_20260909/dd.jsonl),
[base test](receipts/0_main_20260909/verify.log), and
[loaded-library test](receipts/0_main_20260909/native-values.log) receipts are
committed alongside this record. Generated per-case fixtures can be reproduced
with scripts/12_features.sh.

The PostgreSQL comparison and packaged-library checks in
[46_feature_acceptance.md](46_feature_acceptance.md) identify their earlier
binary and source manifest; this commit verification did not rerun PostgreSQL.
The accepted query contract and explicit semantic limits remain in README.md.

CI adds Linux/macOS build and execution checks plus PostgreSQL/pg_ivm coverage.
Remote CI status must be read from the workflow run after pushing.
