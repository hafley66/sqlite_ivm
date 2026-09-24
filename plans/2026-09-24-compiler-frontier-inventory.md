# Compiler-to-frontier inventory: Sprefa `2_partial` and c2 eval cases

Date: 2026-09-24. Lane: `feature/frontier-compiler-inventory` (boop).
sqlite_ivm worktree HEAD: `73437ee` ("Promote SQLite frontier engine from ISO lab").
Sprefa sibling HEAD: `f468cbbe` (READ-ONLY; its `Cargo.lock` is dirty and was never written).

## Question

What exactly does the Sprefa compiler (DL7) emit to SQL for the `2_partial`
fixture and the c2 eval family, what does that SQL look like, and how far does
it get through the real SQLite frontier engine (install / settle / read)?

## Binaries (provenance)

- `dl8` — built from a `git archive HEAD` snapshot of sprefa at `f468cbbe`
  (scratch `target/sprefa-src/`, gitignored; two scratch-only build fixes:
  appended `[workspace]` table so cargo does not join the sqlite_ivm
  workspace, and a `hafley-rs` symlink). Binary:
  `/Users/chrishafley/.agent/lanes/feature-frontier-compiler-inventory/target/release/dl8`.
- `libsqlite_ivm.dylib` — built from the byte-clean **main** sqlite_ivm
  checkout at `73437ee` (the worktree's committed `Cargo.lock` pins absolute
  `hafley-rs-wt` paths and cannot build without editing the lock, which is out
  of scope). Binary: `target/extension/release/libsqlite_ivm.dylib`
  (relative to the worktree; inside the boop-lane target dir).
- sqlite3 CLI: `/opt/homebrew/opt/sqlite/bin/sqlite3` — `/usr/bin/sqlite3`
  has `OMIT_LOAD_EXTENSION` and cannot `.load`.

## Commands (all from the worktree root)

```sh
export DL8=/Users/chrishafley/.agent/lanes/feature-frontier-compiler-inventory/target/release/dl8
export SPREFA=/Users/chrishafley/projects/sprefa
export SQLITE_IVM_LIB="$PWD/target/extension/release/libsqlite_ivm.dylib"
export SQLITE3=/opt/homebrew/opt/sqlite/bin/sqlite3
OUT=bench/frontier/compiler/artifacts

bash bench/frontier/compiler/1_compile_emit.sh "$DL8" "$SPREFA" "$OUT"
python3 bench/frontier/compiler/2_inventory.py "$OUT"
python3 bench/frontier/compiler/3_install_update.py "$OUT" "$SQLITE_IVM_LIB" 3
python3 bench/frontier/compiler/4_frontier_probe.py "$OUT" "$SQLITE_IVM_LIB"
```

`1_compile_emit.sh` shells out to real `dl8` (`dl8 compile`, `dl8 emit sqlite`,
`dl8 eval --db`, `dl8 eval`) against `fixtures/sqlite_emit/{0_union_filter,
1_transitive,2_partial}.dl7` and `oracle/eval/c2_partial_{0..7}.json`. The
`DL8_ENGINE=sqlite` parity leg loads the dylib through dl8, so
`SQLITE_IVM_LIB` must be set or that leg fails with
`not_built_yet("eval open")` (0/8) — the engine is an external sqlite_ivm
extension, not a built-in.

Prereq learned on the way: **the CLI must set `PRAGMA
recursive_triggers=ON; PRAGMA trusted_schema=ON;` before `.load`** — the
extension refuses otherwise ("sqlite_ivm requires recursive_triggers=ON and
trusted_schema=ON"). All runners encode this order.

## 1. Emit: DL7 -> SQL receipts

`receipt.tsv` (`case, leg, diagnostics`):

| case | emit diagnostics | eval-db | eval-sqlite |
|---|---|---|---|
| 0_union_filter | 0 | 0 | 0 |
| 1_transitive | 0 | 0 | 0 |
| 2_partial | 3 | 0 | 0 |

- `bench/frontier/compiler/artifacts/0_union_filter.sql` — 3257 bytes,
  5 `CREATE VIRTUAL TABLE ... USING sqlite_ivm('...')` statements.
- `bench/frontier/compiler/artifacts/1_transitive.sql` — 478 bytes,
  1 view (recursive).
- `bench/frontier/compiler/artifacts/2_partial.sql` — **0 bytes**. Emit
  refuses all 3 rules (`2_partial.emit.json`, `phase: "emit"`,
  `emit_sqlite_unsupported`):

| rule | refusal reason |
|---|---|
| `excluded_request` | depends_on `selected_request` |
| `history_request` | relation `HistoryV1` |
| `selected_request` | relation `Partial` |

This is the headline DL7->SQL result: `2_partial` produces no SQL at all;
every rule is refused at emit, before any engine stage.

## 2. Inventory of the emitted SQL

`python3 bench/frontier/compiler/2_inventory.py` (parser in `ivm_emit.py`;
emitted shape: `CREATE VIRTUAL TABLE "<prefix>.<Rel>_v<arity>" USING
sqlite_ivm('<module arg>')` where the module arg is
`WITH [RECURSIVE] <ctes> SELECT <cols> FROM <target>`; rule bodies are the
top-level UNION branches inside CTE bodies; `dep_depth` is the longest
CTE-name reference chain; `program_rules` counts `program.rules` in the
compile output, prelude included).

| case | view_installs | derived_heads | rule_bodies | repeated_ctes | dep_depth | program_rules | sql_bytes |
|---|---|---|---|---|---|---|---|
| 0_union_filter | 5 | 5 | 7 | 1 | 2 | 134 | 3257 |
| 1_transitive | 1 | 1 | 3 | 0 | 2 | 130 | 478 |
| 2_partial | 0 | 0 | 0 | 0 | 0 | 138 | 0 |

- `repeated_ctes = 1`: the `Heavy_v2` CTE body is inlined verbatim into the
  `Tagged_v2` view (upstream view inlined as a CTE), so the identical body
  text occurs in two views.
- `dep_depth = 2` for both emitting cases: e.g. `Tagged_v2 -> Heavy_v2 ->
  Weight_a2` and `Reach_v2 -> Reach_r2 -> Edge_a2`.
- `1_transitive`'s 3 rule bodies = recursive anchor + recursive step + the
  `Reach_v2` read-off projection.

Representative EQP (`artifacts/1_transitive.eqp.txt`, real sqlite3
`EXPLAIN QUERY PLAN` on the module-arg query):

```
QUERY PLAN
|--CO-ROUTINE Reach_r2
|  |--SETUP
|  |  `--SCAN t0
|  `--RECURSIVE STEP
|     |--SCAN t0
|     `--SEARCH t1 USING COVERING INDEX sqlite_autoindex_1_transitive.Edge_a2_1 (c0_term=?)
`--SCAN Reach_r2
```

0_union_filter's `FromA_v1` join chain uses PK/covering-index searches and a
`USE TEMP B-TREE FOR DISTINCT` (`artifacts/0_union_filter.eqp.txt`).

## 3. c2 A/B parity (Rust engine vs sqlite dylib engine)

`artifacts/c2_parity.tsv` — 8/8 for both engines:

| case | rust | sqlite |
|---|---|---|
| c2_partial_0..7 | match (8/8) | match (8/8) |

The sqlite leg runs dl8 with `DL8_ENGINE=sqlite` plus `SQLITE_IVM_LIB` at the
dylib. Without the env var it is 0/8 (`not_built_yet("eval open")`) — recorded
because it pins the engine's loading mechanism.

## 4. Install / update / read on the real dylib

`artifacts/install_update.tsv` (3 reps; one sqlite3 process per rep: install
DDL -> read -> DELETE of first seed row -> read -> re-INSERT of the identical
row -> read -> `wal_checkpoint(TRUNCATE)` -> `.stats`).

| case | install ms (median) | delete ms | insert ms | hash install = hash after re-insert | RSS bytes | sqlite mem cur/peak | db bytes | wal |
|---|---|---|---|---|---|---|---|---|
| 0_union_filter | 47.28 | 0.031 | 0.026 | yes (`04806a5c548f72df`) | 12.4–13.5 M | 5338864 / 5638784 | 7798784 | 0 |
| 1_transitive | 3.04 | 0.044 | 0.003 | yes (`623b911d32fa82ba`) | 8.1–8.7 M | 3340976 / 3604912 | 2490368 | 0 |
| 2_partial | — no SQL to install — | | | | | | | |

Correctness signal: `hash_after_insert == hash_install` for both cases and
all reps (view content fully restored after the delete/re-insert round trip;
hashes are stable across reps). The DELETE settles (view loses exactly the
derived tuples of the deleted seed) and the re-INSERT restores them.

## 5. Frontier-engine probe of the emitted SQL

`python3 bench/frontier/compiler/4_frontier_probe.py` drives the promoted
engine API `SELECT sqlite_ivm_frontier_install('name', '<select>')` over a
real `.load`, per candidate shape, with exact error texts in
`artifacts/frontier_probe.tsv`. Program names must be plain identifiers, so
the runner installs as `p_<case>` (stems start with a digit).

**Every emitted shape is rejected; none installs as-is.** Error taxonomy
(stage in `[..]`):

| candidate | stage | exact error |
|---|---|---|
| `full_ddl`, `ddl` (CREATE VIRTUAL TABLE...) | `parse/CREATE` | unsupported: a program must start with SELECT |
| `query` (WITH ... SELECT ... UNION ...) | `parse/WITH` | unsupported: a program must start with SELECT |
| `outer_arm` (bare quoted name) | `parse/` | unsupported: a program must start with SELECT |
| `cte_body` recursive anchor (`SELECT 0, ...`) | `parse/Some(Number("0"))` | unsupported: identifier expected |
| `cte_body` with WHERE (e.g. `Reach_v2` read-off) | `plan/` | unsupported: filters are not a supported shape |
| `cte_body` with `SELECT DISTINCT` | `plan/` | unsupported: DISTINCT is set semantics; the union root already dedups |
| `cte_body` inlined `Heavy_v2` (verbatim upstream body) | `parse/>` | unsupported: character in program SQL |

Bodies that get past the parser die in the planner on exactly two shapes:
`DISTINCT` and `WHERE` filters. The recursive anchor additionally dies in the
lexer on the `0` literal.

**Earliest executable slice at body granularity: none.** No emitted CTE body
or outer arm installs, settles and reads.

Liveness control (`minimal_derived` rows in the TSV): a hand-stripped body in
the engine's accepted grammar — `SELECT t0."c0_term", t0."c1_term" FROM
"<first store table>" AS t0`, no DISTINCT/WHERE — **installs, settles and
maintains on both cases** ("installed as p_<case>; frontier rows after delete:
0, after re-insert: 1"). The frontier engine itself is working end to end
through the dylib; the gap is precisely the compiler's emitted body shapes
(set semantics + filters + literals), not the engine.

## Calibration caveat

The runnable install/update/probe legs run on `1_transitive` and
`0_union_filter`, not `2_partial`, because emit refuses every `2_partial`
rule and there is no SQL to install. These two are calibration cases for the
pipeline mechanics (real dylib, real install/settle/read, hash round trips);
they are labeled as such throughout. The `2_partial`-specific result is the
emit refusal table above.

## Files

- `bench/frontier/compiler/ivm_emit.py` — emit-artifact parser (scanner-based
  CTE/outer-SELECT parser; `load_emit`, `parse_ddl`, `parse_query`,
  `referenced_relations`, `split_top_level`).
- `bench/frontier/compiler/1_compile_emit.sh` — compile -> emit -> eval
  receipts (`receipt.tsv`, `c2_parity.tsv`, per-case `.json/.sql/.emit.json/
  closure*.json/store.sqlite`).
- `bench/frontier/compiler/2_inventory.py` — inventory TSV + per-case
  `.inventory.json`.
- `bench/frontier/compiler/3_install_update.py` — install/update/read timings,
  content hashes, RSS, allocator stats, DB/WAL bytes (`install_update.tsv`,
  `.eqp.txt`).
- `bench/frontier/compiler/4_frontier_probe.py` — frontier engine probe
  (`frontier_probe.tsv`).
- `bench/frontier/compiler/artifacts/` — all receipts. `*.store.sqlite` are
  binary scratch DBs (not committed).

## Known measurement caveats

- Install times include whole-connection startup effects only via
  `/usr/bin/time` RSS (reported separately); per-statement install time is the
  sum of the `CREATE VIRTUAL TABLE` `.timer` lines.
- The probe rejects are the engine's own diagnostics (dl8's sqlite engine and
  the standalone frontier API share the parser/planer stages), captured
  verbatim; no error text was paraphrased.
