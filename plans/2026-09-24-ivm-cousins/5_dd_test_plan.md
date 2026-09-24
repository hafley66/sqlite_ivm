# DD step 1 test plan, draft 2

Draft 1 plus three reviews: DD semantics (D-cases), adversarial implementer (K-gates),
minimal reach (S-scripts, random harness). Goal: a library-developed lab. One
differential-dataflow 0.25.1 engine takes a `Program` as data and signed frontiers, returns
signed deltas.

"No recompute-and-diff" binds the ENGINE. The test oracle is SQLite recompute by design.

## What breaks if wrong

A row stays visible after its last support is gone, a row is counted twice, a group total
drifts, an in-batch cancel leaks flicker, recursion keeps a self-supporting row, or the engine
passes by recognizing the test instead of computing.

## Unit under test and seam

Engine API: `install(&Program)`, `settle(Frontier) -> Delta`, `snapshot(RelId)`. No other `pub fn`.
Seam: `settle`. Every script is one `.sql` oracle file: the harness executes its steps in SQLite,
derives each `Frontier` from the same steps (before/after diff of source tables), and derives the
expected delta from consecutive result snapshots. Input and expected share one file. Committed
TSVs are regenerated from the `.sql` in CI and diffed; the engine is never in that pipeline.
Every step is replayed in order; a skipped step fails the run.

## Contract decisions

| # | decision | status | cases |
|---|---|---|---|
| Q1 | raw API: delete of an absent row is a no-op, like SQL `DELETE` matching 0 rows; the row never reaches DD; a `tracing::warn!` event records tick, relation, row | decided 2026-09-24 | D4 expects empty delta plus exactly one warn event; D5 expects `+(9,100)` once |
| Q2 | raw API: insert of a present row is rejected with an error naming relation and row | decided 2026-09-24 | D6 asserts the error |
| Q3 | global aggregate over empty input emits no row, matching `HAVING COUNT(*) > 0` (`_7_sqlite.rs:1775`) | decided 2026-09-24 | D20 expects empty, D21 expects `+(1,5)` then `-(1,5)` |
| Q4 | `seed + SUM(x)` lowers to `Reduce Sum` then `Mfp` adding the seed; `Agg` stays minimal | decided 2026-09-24 | S-G |

## Named scripts

| script | program | steps | covers |
|---|---|---|---|
| S-A | access: union of grant and membership⋈permission | A0 A1 A2 A3 A4 A5 A8, all from `2_oracle.sql` | Join, Union, Threshold, support 2→1 silent, last-support retraction, insert cross-term, update = delete+insert |
| S-G | `team, count, sum, min, max` + 2nd output `count(*)` no GROUP BY | G0 G1 G2 G3 G4 from `3b_aggregate.sql`, widened | Reduce count/sum/min/max, group move in one batch, empty group vanishes, empty-key Reduce, 2 outputs in one install |
| S-W | access + aggregate programs, raw weights | D1 `+m(6,10) -m(6,10)` one batch → empty; D2 delete+reinsert live row → empty; D3 cost 7→7 → empty; D7 `m(7,70)`×2 `p(70,700)`×3, retract one at a time → `+`, empty, `-`; D8 both join sides retracted in one batch → one `-`; D9 key support >1 both sides, `-p(10,100)` → `-(1,100) -(2,100)`; D13 install with empty sources; D17 group nets to zero in batch → empty; D18 sum exactly 0 → `+(40,2,0)`; D19 sum returns to 0 → `-(40,1,5) +(40,2,0)` | consolidation, join weight = R·R2, retraction cross-term, sum not encoded as diff |
| S-N1 | `m(p,t) WHERE t > 10 AND NOT EXISTS g(p,_)` | f0 `+(3)`; `-g(2,100)` → `+(2)`; `+g(3,300)` → `-(3)`; `m(3,30)→m(3,5)` → none; D27 right multiplicity 2 → absent, no −1 row; D28 right 2→1→0 → empty, `+(1)` | Antijoin both directions, Mfp filter crossing, right side distinct before semijoin |
| S-N2 | `reach :- e, NOT blocked; reach :- reach, e, NOT blocked` | `+blocked(3)`, `-blocked(3)`, `-e(2,3)` on cycle 1→2→3→1; D22 two paths to d, delete one; D23 2-cycle terminates; D24 seed weight 1; D25 self-loop delete; D26 delete+re-add in one batch → empty | LetRec, cycle deletion, stratified antijoin outside SCC, no self-supporting rows |
| S-N3 | `team, max(name)` over text; argmax `ORDER BY cost DESC, job ASC` | tie `c,c` survives one delete; runner-up after both; D29 tie resolved by pinned order; D30 winner moves team in one batch | TopK 1, tie collapse, TopK group move |
| S-N4 | `gp(x,z) :- p(x,y), p(y,z)` | `+p(1,2) +p(2,3) +p(2,2)` one batch; `-p(2,2)` | self-join Δ⋈Δ on one relation |
| S-N5 | `n(0). n(y) :- n(x), y = x+1`, limit 64 | one step | LetRec limit, IntAdd map inside recursion, depth-exceeded row |
| S-T | access program | D10 `+g(5,500)` then `-g(5,500)` as two frontiers → `+` then `-`; D11 2000 rows in one frontier, `snapshot` equals delta sum; D12 snapshot right after first settle | advance_to per frontier, probe gate, flush |
| S-I | access program | D14 equal rows in 3 relations; D15 `g(1,23)` vs `g(12,3)`; D16 i64 extremes | RelId tag kept, interning not by concatenation, full i64 range |

Invariant after every step of every script (D31): every snapshot weight is +1 for set outputs;
every `-r` in a delta matches a row visible before; raw `Delta` has no repeated `(RelId, Row)` and
no `w == 0`, checked before any harness sort or merge (K14).

## Random differential harness

| item | detail |
|---|---|
| programs | the S-A, S-G, S-N1, S-N2, S-N4 programs, plus random well-typed `Program` DAGs (random ops, arities, depth) |
| data | values 0..3 (cycles common), 20-50 frontiers of 1-4 rows, weights in {±1, ±2}, cancelling pairs, chains 50-500 for LetRec |
| oracle | IR→SQL printer in the test crate, sharing no code with any engine; SQLite full SELECT per tick |
| metamorphic | permute RelId/NodeId and rename rels (K4); bijection on key columns, costs ×k so sums ×k (K5); random start tick, split one frontier into k and compare consolidated sums (K7) |
| seeds | each failing seed written to a corpus dir with its oracle TSV and replayed first on later runs |

## Gates against cheating

| gate | kills | mechanism |
|---|---|---|
| K1 scaling | recompute-and-diff | one-row change at N=1e3 and N=1e5: `differential/arrange` batch record totals equal within a constant, read from a logger the harness registers |
| K2 no rebuild | dataflow per settle | timely `Operates` events only during `install`; 0 across all `settle` |
| K8 external counters | self-reported work | counters only from timely/differential log streams; engine exposes none |
| K9 census | ops outside DD | multiset of DD operator names from `Operates` equals the table derived from the `Op` list |
| K10 real collections | whole relation as one datum | compile-time assert `DdRel::C == VecCollection<'_, u64, Row, i64>`; distinct-key count >1 on a 1e4-group input |
| K11 output scaling | O(output) snapshot diff | counting global allocator: bytes per `settle` constant within a factor from 1e3 to 1e5 output rows |
| K6 K20 K27 grep and deps | engine reads TSV, global state, SQLite inside DD engine | engine src: no `include_str!`, `include_bytes!`, `std::fs`, `.tsv`, `plans/`, `static`, `thread_local!`, `OnceLock`, `Connection`, fixed rel names; `cargo tree -e normal` has no `rusqlite`/`libsqlite3-sys`; tests run with cwd = empty temp dir |
| K20 isolation | shared state | two engines, different programs, interleaved settles, each equals its solo run |
| K25 snapshot source | snapshot from host-side delta sums | `snapshot` reads the `TraceAgent` cursor; compared to SQLite including intermediate derived rels |
| K26 workers | single-worker timing luck | whole suite also at `timely::Config::process(4)` |

## Untested

| gap | why |
|---|---|
| transaction rollback, savepoint (A6, A7, G5) | Raw host has no transactions; return with the Plugin host step |
| Window, Delay, standalone Negate | Window and Delay parked by user; Sprefa emits none of the three |
| performance numbers | step 9, after every gate here passes |
