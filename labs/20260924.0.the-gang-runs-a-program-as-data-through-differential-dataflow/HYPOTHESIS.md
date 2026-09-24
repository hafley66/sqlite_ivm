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
| `src/3_sqlite.rs` | SQLite engine behind feature `sqlite` (in progress) |
| `oracle/*.sql` + `*.program.json` | one step file drives frontier and expected delta; SQL steps or raw `+/-` lines |
| `tests/0_scripts.rs` | named scripts S-A … S-N5, S-T, S-I |
| `tests/2_random.rs` | random programs and frontiers vs an IR→SQL printer (in progress) |
| `tests/3_gates.rs` | K1, K2, K6/K27, K9, K20 from timely/differential logs |

## Measurement

```
cd labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow
CARGO_TARGET_DIR=../20260923.3.dd-inside-sqlite/target cargo test --offline -j 2
```

Recorded 2026-09-24, debug build, DD engine:

| suite | result |
|---|---|
| `0_scripts` | 10 passed: S-A, S-G, S-W ×2, S-N1, S-N2, S-N3, S-N4, S-N5, S-T/S-I |
| `3_gates` | 5 passed |
| K1 | one-row change arranges the same rows at 1e3 and 3e4 loaded; a change keyed to all loaded rows arranged 3002 vs 90002 and failed the gate |

Sabotage runs, each turned red then restored: threshold passing weights through (S-A step 0);
guard forwarding absent deletes (S-W); antijoin without right-side threshold (S-N1 d27); TopK
ignoring `desc` (S-N3 f0); LetRec without in-loop threshold (S-N2 diverges, 10s watchdog).

## Open

| item | state |
|---|---|
| `LetRec.limit`, nested LetRec, Window, Delay | explicit `Unsupported` |
| K26 multi-worker | engine uses `execute_directly`; the boundary guard reads one worker's trace |
| K11 allocation scaling | not measured |
| SQLite engine equivalence | worker in progress |
| paired DD vs SQLite timing and RSS | after equivalence |

## Invalidates

If the SQLite engine matches on every script and random seed, `crates/frontier-engine`'s SQL
parser front end (`plan.rs`) is replaceable by `lower` over this IR.

## Verdict

DD half: holds on every named script and gate above. SQLite half: unrun.
