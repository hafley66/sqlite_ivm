# unify: refactor, delete, lift

Pass one: inventory. Nothing here is decided. Each later pass takes one
section, reads the code, and replaces the guess with a receipt.

## Inventory (`git ls-files`, counted 2026-09-20)

| tree | files | lines | languages | role today |
|---|---|---|---|---|
| `src/` | 9 | 5.6k | rs | the crate |
| `tests/` | 12 + support + fixtures | 3.7k | rs, json | battery |
| `examples/` | 2 | 0.4k | rs | bench-feature entry points |
| `bench/` + `bench/shared/` | 60 | 11k | mjs, sh, rs, py, pl, sql | fixtures, arms, runners, reports |
| `scripts/` | 17 | | sh, mjs | build, crud smoke, shootout wrappers |
| `labs/` | 4 labs + protocol | | md, rs | dead by law on landing |
| `probes/` | 2 | | | dead by law on landing |
| `docs/`, `plans/`, `issues/` | | | md | keep |

Count command: `find bench scripts -type f -not -path '*/target/*' \
-not -path '*/.work/*' -not -path '*/receipts/*' | sed 's/.*\.//' | sort | uniq -c`.

## Candidate moves

Each row is a fork for Chris. Order is by how much it deletes.

| # | move | deletes | risk |
|---|---|---|---|
| 1 | one Rust bench binary owns fixture gen, arms, timing, TSV | `bench/` mjs/sh/py/pl (55 files) | shootout numbers must reproduce first |
| 2 | `scripts/*_crud.sh` become tests or die | 17 sh | crud smoke may already be `tests/1_maintenance.rs` |
| 3 | `labs/`, `probes/` condense into `docs/failure-modes.md` rows and fixtures, then delete | 45 files | read each README once |
| 4 | `src/1a_relational.rs` (1.6k) and `src/0b_relational.rs` (2.0k): split by node kind, SQL text built once per view | none, restructures 3.6k | perf item "SQL rebuilt per drain" rides on it |
| 5 | lift to `hafley-rs` crates | see below | api surface |

## Lift candidates

| code | lives | lift to | why |
|---|---|---|---|
| fixture generation + oracle (`bench/shared/30_circuit_workload.mjs`) | mjs | new crate `ivm-fixtures` in hafley-rs, serde JSON out | pg_ivm, DD, DBSP, sqlite arms all read one struct |
| statement cost script (`scripts/statement-costs.sh`) | sh + duckdb | `hafley-observe` subcommand or example | observe already owns the span log format |
| `tests/support/0_database.rs` | rs | stays; check for twin in `sprefa` tests | |
| bulk trigger install | already `sqlite-bulk-trigger` | done | |
| SQL builder helpers (scratch names, width suffix) | `src/1a_relational.rs` top | stays unless a second crate builds SQL text | |

## Pass plan

| pass | reads | produces |
|---|---|---|
| 2 | every `bench/*.md`, `labs/*/README`, `probes/*` | keep/condense/delete list with a one-line reason each |
| 3 | `src/0b_relational.rs`, `src/1a_relational.rs` | module map by node kind; per-view statement program shape |
| 4 | `bench/shared/*.rs`, `examples/*.rs` | bench binary design, arm trait, fixture struct |
| 5 | hafley-rs crate list | lift list with crate names and public API |

## Facts checked this pass

- `origin/main` `Cargo.toml:23` already takes `hafley-observe` from the
  `hafley` registry; the path deps are only on `perf/drain-costs`.
- `tests/14_scale.rs` runs 206s in the debug battery on `perf/drain-costs`;
  breaks the 10-second law; must be `#[ignore]` or a bench.
