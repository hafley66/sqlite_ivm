# Compiler-to-frontier inventory: Sprefa `2_partial` and c2 eval cases

Date: 2026-09-24 (rev 2 after review). Lane: `feature/frontier-compiler-inventory` (boop).
sqlite_ivm worktree HEAD: `73437ee`. Sprefa sibling HEAD: `f468cbbe` (READ-ONLY; its dirty
`Cargo.lock` was never written).

## 0. Current emit failure for `2_partial` (verified, leads the report)

`dl8 compile tests/fixtures/2_partial.dl7` exits 0. `dl8 emit sqlite` on the
compiled program **exits 1**:

- stderr: **empty** (`artifacts/2_partial.emit.stderr.txt`, 0 bytes — captured
  per command; the failure reports on stdout, not stderr).
- stdout: a diagnostics document, 1427 bytes (`artifacts/2_partial.emit.json`),
  `phase: "emit"`, three `emit_sqlite_unsupported` payloads:

| rule | refusal payload |
|---|---|
| `excluded_request` | `depends_on: selected_request` |
| `history_request` | `relation: HistoryV1` |
| `selected_request` | `relation: Partial` |

- `artifacts/2_partial.sql` is **0 bytes because no view was emitted** — it is
  the derived artifact of a failed emit, **not** "generated SQL of length 0".
  `receipt.tsv` records `emit 2_partial 1` (per-leg exit codes, plus a
  `compile` leg); the case is not hidden and not counted as emitted anywhere.

The two nearest emitting cases (`1_transitive`, `0_union_filter`) emit with
rc=0 and produce the SQL analyzed below.

## 1. Historical captured SQL (prior lane; NOT reproducible by current emit)

These files live in the main checkout `plans/costs/` and predate this lane.
They are recorded separately so the current emit failure is not silently
compared against them.

- `56_2partial_138_rule_view.sql` — **committed in this lane at
  `bench/frontier/compiler/artifacts/history/`, provenance in
  `history/PROVENANCE.md`**. Source of record: `plans/costs/56_2partial_138_rule_view.sql`
  in the main sqlite_ivm checkout, copied verbatim: 88,023 bytes,
  sha256 `8e71fe0760f028b4b2a95b6eb13b94631402012a7ec77bed3ab6235af6b6fe1a`,
  captured 2026-09-23 10:11. Shape: ONE whole-program
  `CREATE VIRTUAL TABLE "program" USING sqlite_ivm('WITH RECURSIVE ...')`
  covering the 138-rule program, store tables prefixed `main.n94677_a2`.
  This is the earlier whole-program lowering. The current `dl8 emit sqlite`
  (sprefa `f468cbbe`) refuses every `2_partial` rule instead — the two are
  different compiler modes and the current failure does not invalidate the
  historical capture, nor vice versa.
- `48_c2_standalone_replay.json` — c2 replay evidence for `c2_partial_3`
  (case PROGRAM compiled under plugin `1d6d8d6`): 78,219-byte view SQL
  (sha `a4506115...`), standalone vs compiler eval output hashes, recorded
  gates (27 passed compiler-emitted eval-oracle contract, 86 nextest, etc.).
- `49_c2_fixpoint_native_keys.html` — fixpoint/native-keys evidence HTML for
  the same older pipeline.

## 2. c2 parity: programs under two engines (not an emitted-SQL comparison)

The `oracle/eval/c2_partial_*.json` files are case **programs**. This lane
runs each program under dl8's Rust engine and under
`DL8_ENGINE=sqlite` (+ `SQLITE_IVM_LIB` at the real dylib) and compares both
closures against the frozen v7 oracle (`c2_parity.tsv`): **8/8 match for both
engines**. Without `SQLITE_IVM_LIB` the sqlite leg is 0/8 with
`not_built_yet("eval open")` — the engine is an external sqlite_ivm
extension. This says the two engines agree on these programs; it makes no
claim about emitted SQL for them.

## 3. Inventory of the emitted SQL (the two emitting cases)

Parser: `0_ivm_emit.py` (numeric reading prefix per repo convention; imported
via `importlib` because the module name starts with a digit). Emitted shape:
`CREATE VIRTUAL TABLE "<prefix>.<Rel>_v<arity>" USING sqlite_ivm('WITH
[RECURSIVE] <ctes> SELECT <cols> FROM <target>')`; rule bodies are the
top-level UNION branches inside CTE bodies; `dep_depth` = longest CTE-name
reference chain; `program_rules` counts `program.rules` (prelude included).

| case | views | derived_heads | rule_bodies | repeated_ctes | dep_depth | program_rules | sql_bytes |
|---|---|---|---|---|---|---|---|
| 0_union_filter | 5 | 5 | 7 | 1 | 2 | 134 | 3257 |
| 1_transitive | 1 | 1 | 3 | 0 | 2 | 130 | 478 |
| 2_partial | 0 | 0 | 0 | 0 | 0 | 138 | 0 (failed emit, see §0) |

- `repeated_ctes = 1`: the `Heavy_v2` CTE body is inlined verbatim into the
  `Tagged_v2` view.
- `dep_depth = 2`: e.g. `Tagged_v2 -> Heavy_v2 -> Weight_a2`,
  `Reach_v2 -> Reach_r2 -> Edge_a2`.
- `1_transitive`'s 3 rule bodies = recursive anchor + step + `Reach_v2`
  read-off projection.

Parser hardening after review: the outer SELECT is now split at the single
depth-0 ` FROM ` (the old greedy `.* FROM .+$` could mis-split a
parenthesized UNION target); a parenthesized UNION target or a second
top-level FROM raises instead of guessing. Parse check (this session): all
committed `.sql` artifacts parse; `SELECT x FROM (SELECT ...)` raises
"parenthesized UNION outer target not produced by the current emitter";
`SELECT x FROM a FROM b` raises "unrecognized outer SELECT".

Representative EQP (`artifacts/1_transitive.eqp.txt`, real sqlite3
`EXPLAIN QUERY PLAN`):

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

`0_union_filter`'s `FromA_v1` chain uses PK/covering-index searches plus
`USE TEMP B-TREE FOR DISTINCT` (`artifacts/0_union_filter.eqp.txt`).

## 4. Install / update / read on the real dylib (fail-closed runner)

`3_install_update.py` runs one sqlite3 process per rep with `-bail`; the TSV
carries `rc`, `status` (ok / `failed: <exact error>`), `hash_valid`, and
timings are `-` on failure. The seed row is saved to a TEMP table and
restored with `INSERT INTO src SELECT * FROM ivm_saved` inside the same
connection — a typed round-trip; no CLI pipe text is ever re-parsed as SQL.
Subprocess status is fail-closed: `run_cli` returns the process rc, and any
non-zero rc or error text marks the rep `failed` with blank timings.

**Hashes are INTEGER-ONLY content hashes.** Two fail-closed assertions make
them exact for these fixtures: (1) every cell of the mutated source table is
asserted integer before the rep runs (`assert_integer_cells`; the
round-trip table `Edge_a2` is all INTEGER in both stores); (2) any
non-integer row in a view's output marks the rep
`failed: N non-integer output row(s)` instead of hashing it. Read-only TEXT
inputs the views join (`sym`/`term` arenas) are outside the hash domain; a
non-integer cell would fail the rep, not silently shift a hash.

| case | install ms (median) | delete ms | insert ms | hash install = hash after re-insert | RSS | sqlite mem cur/peak | db bytes | WAL |
|---|---|---|---|---|---|---|---|---|
| 0_union_filter | 47.645 | 0.002 | 0.002 | yes (`04806a5c548f72df`) | 12.0 M | 5339152 / 5639072 | 7798784 | see note |
| 1_transitive | 3.357 | 0.042 | 0.003 | yes (`623b911d32fa82ba`) | 8.2–8.5 M | 3345872 / 3609808 | 2490368 | see note |
| 2_partial | — nothing to install (emit refused) — | | | | | | | |

- Content hashes are identical to the pre-assertion runner — the typed
  round-trip restored exactly what the earlier path restored,
  cross-validating both. `hash_valid = yes` on every rep.
- **WAL note:** the stores do not use WAL journal mode, so no `-wal` file
  is ever created; `wal_bytes_post_commit` is 0/absent and **WAL pressure
  was NOT measured** (the column records the pre-checkpoint file state).

## 5. Frontier-engine probe (exact accepted/rejected)

`4_frontier_probe.py` drives `sqlite_ivm_frontier_install(name, select)`
over a real `.load`, per candidate shape (`frontier_probe.tsv` has verbatim
errors). Program names must be plain identifiers → installed as `p_<case>`.
Source-table candidates are filtered to tables that actually exist in the
store's `sqlite_master` (a recursive CTE's self-name previously leaked into
the candidate list).

**Every emitted shape is rejected; none installs as-is:**

| candidate | stage | exact error |
|---|---|---|
| `full_ddl`, `ddl` (CREATE VIRTUAL TABLE...) | `parse/CREATE` | unsupported: a program must start with SELECT |
| `query` (WITH ... SELECT ...) | `parse/WITH` | unsupported: a program must start with SELECT |
| `outer_arm` (bare quoted name) | `parse/` | unsupported: a program must start with SELECT |
| recursive anchor (`SELECT 0, ...`) | `parse/Some(Number("0"))` | unsupported: identifier expected |
| read-off with WHERE | `plan/` | unsupported: filters are not a supported shape |
| `SELECT DISTINCT` bodies | `plan/` | unsupported: DISTINCT is set semantics; the union root already dedups |
| inlined `Heavy_v2` body | `parse/>` | unsupported: character in program SQL |

**Earliest executable emitted slice at body granularity: none.**

Liveness/mismatch gates (hand-stripped `SELECT t0."c0_term", t0."c1_term"
FROM <first store table> AS t0`, no DISTINCT/WHERE), install + committed
mutation + snapshot read + fresh recompute **in one connection**:

- `0_union_filter`: installs (`p_0_union_filter`); delete → frontier 0 rows;
  re-insert → frontier 1 row; **fresh recompute of the same body over the
  same tables: 4 rows → `accepted-mismatch`**.
- `1_transitive`: same shape — frontier 1, fresh recompute 5 → mismatch.

Finding: **install does not back-fill the frontier from pre-existing base
rows.** The engine only reflects mutations committed after install. The
earlier "accepted-settled" reading was exactly the stale-snapshot illusion
the review warned about; the mismatch gate now records it as a failure
verdict, not a pass.

**Fresh-process reopen gates (install asserted rc=0, then closed and
reopened):**
- fresh READ of the snapshot: **succeeds** — `reopen-read`,
  `count=0` in both cases. The installed frontier vtab persists in the
  schema and a new process reads it; `count=0` is direct confirmation that
  install does not back-fill pre-existing base rows (fresh recompute of the
  same body: 4 / 5 rows).
- fresh committed mutation (DELETE on the source): **fails** —
  `reopen-mutate-unsupported`, exact error `Parse error near line 4: no
  such module: frontier_p_<prog>_c1` (both cases). The collector module for
  a committed source write is connection-local and not re-registered on
  reopen; the read path persists, the write path does not.

### Replay after frontier bootstrap

The original `frontier_probe.tsv` above records the pre-bootstrap engine.
`5_frontier_probe_after_bootstrap.tsv` replays the same copied stores and
candidates against the rebuilt native plugin at sqlite_ivm `89395fc`.
The emitted SQL still rejects at the same stages, and the hand-stripped
candidate now matches a fresh recomputation after delete and re-insert:

| case | initial/reopened rows | after delete | after re-insert | fresh SELECT | verdict |
|---|---:|---:|---:|---:|---|
| `0_union_filter` | 4 | 3 | 4 | 4 | `accepted-settled` |
| `1_transitive` | 5 | 4 | 5 | 5 | `accepted-settled` |

The fresh-process snapshot read now returns 4 and 5 rows respectively.
The fresh-process source mutation still fails with `no such module:
frontier_p_<prog>_c1`; bootstrap does not reattach the collector.

## 6. Calibration caveat

All runnable legs (install/update/probe) run on `1_transitive` and
`0_union_filter` because `2_partial` emits nothing (§0). They calibrate the
pipeline mechanics only. The `2_partial`-specific results are the emit
refusals in §0 and the historical captures in §1.

## 7. Commands (all from the worktree root)

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

Binaries: `dl8` built from a `git archive HEAD` snapshot of sprefa
(`f468cbbe`; scratch-only `[workspace]` fix + `hafley-rs` symlink, both
outside the repo); `libsqlite_ivm.dylib` built from the clean **main**
sqlite_ivm checkout at `73437ee` with `--target-dir` inside the lane — the
worktree's committed `Cargo.lock` pins absolute `hafley-rs-wt` paths and
cannot build without editing it (environment limitation, not worked around).
sqlite3 CLI: `/opt/homebrew/opt/sqlite/bin/sqlite3` (`/usr/bin/sqlite3` has
`OMIT_LOAD_EXTENSION`). Pragmas `recursive_triggers`/`trusted_schema` must be
set BEFORE `.load` or the extension refuses.

## 8. Files

- `0_ivm_emit.py` — emit-artifact parser (fail-closed; see §3).
- `1_compile_emit.sh` — compile/emit/eval receipts with per-command stderr
  captures (`<stem>.<leg>.stderr.txt`) and per-leg exit codes.
- `2_inventory.py`, `3_install_update.py`, `4_frontier_probe.py` — inventory,
  fail-closed install/update TSV, frontier probe TSV.
- `artifacts/` — all receipts; `*.store.sqlite` binaries stay untracked.
- `artifacts/history/` — the historical whole-program capture
  (`56_2partial_138_rule_view.sql`) plus `PROVENANCE.md` (§1).
