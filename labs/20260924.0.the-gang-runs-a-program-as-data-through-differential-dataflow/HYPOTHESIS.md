# The gang runs a program as data through differential dataflow

## Hypothesis

One differential-dataflow 0.25.1 engine can install any `Program` of the IR in
`plans/2026-09-24-ivm-cousins/4_design.md` as data (no per-program Rust), settle signed
frontiers incrementally, and match a SQLite recompute oracle on every step. A SQLite engine
written against the same `Rel`/`Engine` traits, using relational SQL over delta and state
tables, can then be proven equivalent to it on the same oracles.

Falsified by: any oracle step where the delta or snapshot differs; any gate showing work that
grows with loaded size for a constant-size change; any operator built after install.

## Knob

Which relational operators are expressed as DD operators vs SQL statements, behind one trait.

## Invariants assumed of the input tables

- Source relations are sets (SQLite primary keys). Insert of a present row: the engine rejects
  the frontier (decision Q2). Delete of an absent row: no-op plus `tracing::warn!` (Q1).
- Cells are i64. Text would be interned to i64 ids before reaching an engine (not exercised yet).
- A global aggregate over empty input emits no row (Q3). `seed + SUM` lowers to Reduce + Mfp (Q4).

## Layout

| file | role |
|---|---|
| `src/0_ir.rs` | IR: `Program`, `Op` (MIR names), `Frontier`, `Delta` |
| `src/1_rel.rs` | `Rel` algebra trait, `Engine` trait, `lower`/`lower_node`, scalar `eval` |
| `src/2_dd.rs` | DD engine: `DdRel<'s, T>` generic over timestamp, `Nest` bounds LetRec nesting, boundary guard, trace-cursor snapshots |
| `src/3_sqlite.rs` | SQLite engine behind feature `sqlite`: per-node delta and integrated tables, pre-image join driven from the delta, accumulator tables for Count/Sum, indexed Min/Max, DRed LetRec |
| `oracle/*.sql` + `*.program.json` | one step file drives frontier and expected delta; SQL steps or raw `+/-` lines |
| `tests/0_scripts.rs` | named scripts S-A … S-N5, S-T, S-I |
| `tests/1_sqlite_scripts.rs` | the same scripts on SQLite, DRed cycle vs DD, K1 VM-step gates, EXPLAIN scan gate |
| `tests/2_random.rs` | random programs and frontiers vs an independent IR→SQL printer; permute and split metamorphic checks |
| `tests/3_gates.rs` | K1, K2, K6/K27, K9, K20 from timely/differential logs |

## Measurement

```
cd labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow
CARGO_TARGET_DIR=../20260923.3.dd-inside-sqlite/target cargo test --offline -j 2
```

Recorded 2026-09-24, debug build, `--features sqlite`:

| suite | result |
|---|---|
| `0_scripts` (DD) | 11 passed: S-A, S-G, S-G accumulable, S-W ×2, S-N1, S-N2, S-N3, S-N4, S-N5, S-T/S-I |
| `1_sqlite_scripts` | 14 passed: the same 11 scripts, DRed self-supporting cycle vs DD, K1 VM-step gates (access, team_cost), EXPLAIN scan gate |
| `2_random` | DD 200 + 3000 extra seeds, permute 100, split 100; SQLite 200 + 2×2000 extra seeds; no failures |
| `3_gates` | 5 passed |
| SQLite K1 (VM steps, one change after 1e3 vs 3e4 loaded) | access 824 → 824; team_cost 1421 → 1421 (before the scan fix: 7825 → 210825 and 65421 → 1863421, failing) |

Paired release run, `cargo run --release --offline -j 2 --features sqlite --example 0_paired -- <engine> <workload> <n>`:

| workload | n | DD churn p50 | SQLite churn p50 | DD RSS KiB | SQLite RSS KiB |
|---|---|---|---|---|---|
| access | 1e5 | 36 µs | 32 µs | 82704 | 48800 |
| team_cost (Min/Max, groups of 1000) | 1e5 | 118 µs | 49 µs | 63008 | 26672 |
| team_sum (Count/Sum) | 1e5 | 25 µs | 28 µs | 45360 | 21808 |
| reach_tail (append edge to 300-chain) | 300 | 6521 µs | 1690 µs | 35696 | 14416 |
| reach_middle (cut and rejoin middle edge) | 300 | 53483 µs | 110310 µs | 117776 | 40096 |

Load of the 1e5 frontier: DD 41-88 ms, SQLite 185-499 ms.

DD K1: a one-row change arranges the same rows at 1e3 and 3e4 loaded; a change keyed to all
loaded rows arranged 3002 vs 90002 and failed the gate.

Sabotage runs, each turned red then restored: threshold passing weights through (S-A step 0);
guard forwarding absent deletes (S-W); antijoin without right-side threshold (S-N1 d27); TopK
ignoring `desc` (S-N3 f0); LetRec without in-loop threshold (S-N2 diverges, 10s watchdog).

## Open

| item | state |
|---|---|
| `LetRec.limit`, nested LetRec, Window, Delay | explicit `Unsupported` |
| K26 multi-worker | engine uses `execute_directly`; the boundary guard reads one worker's trace |
| K11 allocation scaling | not measured |
| DD Min/Max | general reduce re-reads the group (118 µs at groups of 1000); hierarchical reduce not built |
| SQLite DRed | assumes the recursive body is monotone in outer inputs; a Negate of an outer relation into an SCC is not detected |
| random harness | no Negate, LetRec, TopK in generated programs; no K5 value bijection |

## Invalidates

If the SQLite engine matches on every script and random seed, `crates/frontier-engine`'s SQL
parser front end (`plan.rs`) is replaceable by `lower` over this IR.

## Verdict

Holds. Both engines match the SQLite recompute oracle on 11 scripts and thousands of random programs, and each other on the DRed cycle case. Both do constant work per constant-size change on access and aggregates. On recursion SQLite is ahead for appends (1.7 ms vs 6.5 ms) and behind for a middle cut (110 ms vs 53 ms).
