# Turso (tursodatabase/turso, formerly Limbo): DBSP-based materialized views

Source snapshot: shallow clone of `https://github.com/tursodatabase/turso`, HEAD
`bec3bbff61232e3573f5e7c446b54d72e5daf549` (2026-09-24). Local copy:
`/private/tmp/claude-501/turso-research/turso`. All paths below are relative to
that repo root. Line numbers are for that commit. Nothing was built or run.

## 1. Feature, syntax, status, first version

| Item | Value | Source |
|---|---|---|
| Feature name | "Live Materialized Views" (IVM) | `cli/manuals/materialized-views.md:5` |
| Syntax | `CREATE MATERIALIZED VIEW [IF NOT EXISTS] view-name AS select-statement;` | `docs/sql-reference/statements/create-materialized-view.mdx` |
| Drop | `DROP VIEW` (drops the internal DBSP state table too) | `docs/sql-reference/statements/drop-view.mdx`; CHANGELOG line 1753 |
| Refresh | none; maintained inside the writing transaction | `create-materialized-view.mdx` "How Incremental View Maintenance Works" |
| Status | experimental, gated | `docs/sql-reference/experimental-features.mdx:15` |
| CLI flag | `tursodb --experimental-views` | `cli/manuals/materialized-views.md:14` |
| Rust SDK | `Builder::...experimental_materialized_views(true)` | `experimental-features.mdx:60` |
| Python SDK | `experimental_features="views,..."` | `experimental-features.mdx:78` |
| Gate check | `validate_materialized` returns ParseError "CREATE MATERIALIZED VIEW is an experimental feature. Enable with --experimental-views flag" | `core/translate/view.rs:19-31` |
| First release | 0.1.4 (2025-08-20): "Initial pass on incremental view maintenance with DBSP", "Implement Aggregations for DBSP views", "move our dbsp-based views to materialized views" | `CHANGELOG.md:2421, 2450, 2457, 2532` |
| Persistence of view state | 0.1.5 (2025-09-10): "Persistence for DBSP-based materialized views" | `CHANGELOG.md:2241, 2307` |
| JOIN + UNION support | 0.2.0 (2025-10-03) | `CHANGELOG.md:1932, 1958, 1960` |
| Announcement blog | "Introducing Real-Time Data with Materialized Views in Turso", 2025-10-09 | https://turso.tech/blog/introducing-real-time-data-with-materialized-views-in-turso |
| Latest release in CHANGELOG | 0.7.0 (2026-07-13), feature still listed experimental | `CHANGELOG.md:3` |

Flag name at 0.1.4 time: UNVERIFIED (shallow clone, no history read).

### Pending rewrite (unmerged)

PR #8015 "Ivm vdbe rewrite" (jussisaurio), created 2026-07-24, state CLOSED,
not merged (https://github.com/tursodatabase/turso/pull/8015). Body states it
replaced the separate operator interpreter with maintenance compiled to VDBE
subprograms, and added LEFT/RIGHT/CROSS joins, non-equi joins, multi-way joins,
derived tables, non-recursive CTEs, TOTAL, FILTER, HAVING. None of that is in
HEAD. Open issue #9325 "Materialized view: an open join does not find a row
after an INSERT into the view's table".

## 2. Source layout

Own implementation. No `dbsp` / Feldera crate dependency (grep of `Cargo.toml`
and `core/Cargo.toml` for `dbsp`/`feldera`: 0 hits). Only reference:
`core/incremental/operator.rs:3` comment "Based on Feldera DBSP design but
adapted for Turso's architecture". `core/incremental/dbsp.rs:1-2`: "Simplified
DBSP integration ... can expand to full DBSP later".

| File | Lines (wc) | Contents |
|---|---|---|
| `core/incremental/mod.rs` | 51 | module list |
| `core/incremental/dbsp.rs` | 549 | `Hash128` (UUIDv5 over typed value string, :12-116), `HashableRow` (:141), `Delta { changes: Vec<(HashableRow, isize)> }` (:202, ordered), `DeltaPair` (:265), `SimpleZSet<T>` (BTreeMap-backed, :300), `RowKeyZSet` (:377) |
| `core/incremental/operator.rs` | 4688 | `IncrementalOperator` trait (:205), `DbspStateCursors` (:24), `create_dbsp_state_index` (:43), `generate_storage_id` (:64), `QueryOperator` (:166) |
| `core/incremental/compiler.rs` | 6256 | `DbspOperator` (:295), `DbspExpr` (:325), `DbspNode` (:339), `DbspCircuit` (:401), `DBSP_CIRCUIT_VERSION = 1` (:397), `DbspCircuit::execute` (:504), `DbspCircuit::commit` (:533), `DbspCompiler::compile` (:940), `DeltaSet` (:244), `WriteRowView` (:41) |
| `core/incremental/expr_compiler.rs` | 553 | compiles projection/filter expressions |
| `core/incremental/filter_operator.rs` | 521 | `FilterOperator`, `FilterPredicate` |
| `core/incremental/project_operator.rs` | 164 | `ProjectOperator` |
| `core/incremental/join_operator.rs` | 727 | `JoinOperator`, `JoinType` (:18) |
| `core/incremental/aggregate_operator.rs` | 3166 | `AggregateOperator`, `AggregateFunction` (:101), storage type tags (:78-80) |
| `core/incremental/merge_operator.rs` | 184 | `MergeOperator`, `UnionMode` (:18) |
| `core/incremental/input_operator.rs` | 63 | `InputOperator` (pass-through source) |
| `core/incremental/persistence.rs` | 409 | `ReadRecord`, `WriteRow` state machines for state-table I/O |
| `core/incremental/view.rs` | 2719 | `ViewTransactionState` (:85), `AllViewsTxState` (:138), `IncrementalView` (:202), `execute_with_uncommitted` (:418), `generate_populate_queries` (:690), `populate_from_table` (:1147), `merge_delta` (:1405) |
| `core/incremental/cursor.rs` | 2078 | `MaterializedViewCursor` (:47): btree + uncommitted overlay |
| `core/translate/view.rs` | - | `translate_create_materialized_view` (:57) |
| `core/translate/logical.rs` | - | `LogicalPlanBuilder` (SELECT -> logical plan, input to DbspCompiler) |
| `core/vdbe/execute.rs` | - | delta capture in `op_insert` (:12502), `op_delete` (:12871); `op_populate_materialized_views` (:15695); MV cursor open in `op_open_read` (:1336, cursor construction ~:1436) |
| `core/vdbe/mod.rs` | - | `ViewDeltaCommitState` (:118), `apply_view_deltas` (:2677), `commit_txn` (:2780) |
| `core/schema.rs` | - | `DBSP_TABLE_PREFIX = "__turso_internal_dbsp_state_v"` (:135), `get_dependent_materialized_views` (:1196) |
| `core/connection.rs` | - | `view_transaction_states: AllViewsTxState` field (:489) |
| Tests | - | `sqlite/conformance/turso-sqltests/materialized_views.sqltest`, `materialized_view_text_arithmetic.sqltest`, `testing/sqltests/tests/ivm-compound-null-filter.sqltest` |

Key signatures:

```rust
// core/incremental/operator.rs:205
pub trait IncrementalOperator: Debug + Send {
    fn eval(&mut self, state: &mut EvalState, cursors: &mut DbspStateCursors) -> IOResultOr<Delta>;
    fn commit(&mut self, deltas: DeltaPair, cursors: &mut DbspStateCursors) -> IOResultOr<Delta>;
    fn set_tracker(&mut self, tracker: Arc<Mutex<ComputationTracker>>);
}
// core/incremental/view.rs:1405
pub fn merge_delta(&mut self, delta_set: DeltaSet, pager: Arc<crate::Pager>) -> IOResultOr<()>;
// core/incremental/view.rs:418
pub fn execute_with_uncommitted(&mut self, uncommitted: DeltaSet, pager: Arc<Pager>,
    execute_state: &mut ExecuteState) -> IOResultOr<Delta>;
```

Every operator is a resumable state machine returning `IOResultOr` (async I/O
model of Turso's pager).

## 3. Storage, timing, delta capture, rollback

### Storage (all on-disk btrees in the main DB file)

`CREATE MATERIALIZED VIEW` emits (`core/translate/view.rs:57-278`):

| Object | Kind | Row format | Line |
|---|---|---|---|
| view data btree | table btree, rowid table, registered in `sqlite_schema` as type `view` with rootpage | `(view columns..., weight)`; key = `HashableRow.rowid` | :98-104, :178-188 |
| `__turso_internal_dbsp_state_v1_<view>` | table btree | `CREATE TABLE (operator_id INTEGER NOT NULL, zset_id BLOB NOT NULL, element_id BLOB NOT NULL, value BLOB, weight INTEGER NOT NULL, PRIMARY KEY (operator_id, zset_id, element_id))` | :108-113, :193-222 |
| `sqlite_autoindex___turso_internal_dbsp_state_v1_<view>_1` | index btree on `(operator_id, zset_id, element_id)` | - | :226-248 |

State-table key encoding:
- `operator_id` column holds `generate_storage_id(operator_id, column_index, op_type) = (operator_id << 16) | (column_index << 2) | op_type` (`operator.rs:64-69`).
- `op_type`: `AGG_TYPE_REGULAR=0b00` (COUNT/SUM/AVG), `AGG_TYPE_MINMAX=0b01` (MIN/MAX; btree ordering serves both), `AGG_TYPE_DISTINCT=0b10` (`aggregate_operator.rs:78-80`).
- `zset_id` = `Hash128` of group key / join key; `element_id` = hash or value (`translate/view.rs:196-200` comment; `join_operator.rs:27-55`).
- Join operator persists both input sides into the state table keyed by join-key hash (`join_operator.rs:26-60`, `commit` :593 `CommitLeftDelta`...).
- Join output rowid = `Hash128::hash_values(combined).as_i64()` (`join_operator.rs` ~:512-516).

In memory: per-connection uncommitted base-table deltas
(`AllViewsTxState`: `view_name -> ViewTransactionState { table_deltas: RefCell<HashMap<table_name, Delta>> }`, `view.rs:85-190`). Compiled circuit lives in `IncrementalView` inside `Schema` (`Arc<Mutex<IncrementalView>>`). Doc comment at `view.rs:192-200` still says "keeps everything in-memory"; code at `compiler.rs:533-640` writes to the btrees (comment predates 0.1.5 persistence; UNVERIFIED which is current intent).

Initial population: `Insn::PopulateMaterializedViews` (`translate/view.rs:273`, `vdbe/execute.rs:15695`) -> `populate_from_table` (`view.rs:1147`), which runs ordinary SQL `SELECT`s per referenced table produced by `generate_populate_queries` (`view.rs:690`) via `conn.prepare`, row at a time, inside `conn.start_nested()/end_nested()`. Comment at `view.rs:~1179-1185` gives the reason: SQL query reuses planner/pushdown instead of reimplementing it on a cursor.

### Delta capture: inside VDBE opcodes (no triggers)

| Opcode | Mechanism | Lines |
|---|---|---|
| `Insert` | sub-state `MaybeCaptureRecord` checks `schema.get_dependent_materialized_views(table_name)`; on UPDATE (non-rowid-change) captures old record; `ApplyViewChange` pushes `delete(old)` then `insert(new)` into each dependent view's `ViewTransactionState`; rowid-alias columns patched from key | `vdbe/execute.rs:12502-12850`, apply at :12769 |
| `Delete` | `MaybeCaptureRecord` reads `cursor.rowid()` + `cursor.record()` before delete; `ApplyViewChange` pushes `delete` | `vdbe/execute.rs:12871-12960`, apply at :12942 |
| `Insert` on WITHOUT ROWID with dependent MV | ParseError "WITHOUT ROWID tables with dependent materialized views are not supported" | `vdbe/execute.rs:~12758-12763` |

The `Insn::Insert`/`Insn::Delete` instructions carry `table_name` so the
opcode can look up dependents at run time.

### When maintenance runs

| Phase | Action | Lines |
|---|---|---|
| DML statement | append (row, +/-1) to per-connection in-memory `Delta` per view per table; no view btree writes | execute.rs above |
| Read of MV inside same tx | `MaterializedViewCursor::ensure_tx_changes_computed` runs `circuit.execute` (eval, no state commit) over the full tx delta, builds `RowKeyZSet uncommitted`, overlays on btree scan; recomputed when `tx_state.len()` grows | `cursor.rs:95-122`, `view.rs:418` |
| Commit | `Program::commit_txn` calls `apply_view_deltas(rollback=false)` first; for each view with deltas: `IncrementalView::merge_delta` -> `DbspCircuit::commit` -> operators' `commit` write state table, then `UpdateView` writes weighted rows into view btree; then clears tx state; then the normal pager/WAL commit proceeds | `vdbe/mod.rs:2677-2776, 2780-2801`; `compiler.rs:533-640` |

View writes therefore land in the same write transaction (same WAL commit) as
the base-table writes, before the pager commit.

### Rollback

- `apply_view_deltas(rollback=true)`: clears `view_transaction_states`, returns (`vdbe/mod.rs:2692-2696`). View btrees/state table were not written during the tx, so nothing to undo on disk.
- Statement-level abort / SAVEPOINT rollback: grep of `view_transaction_states` across `core/` finds call sites only in `database.rs`, `connection.rs`, `incremental/cursor.rs`, `vdbe/mod.rs`, `vdbe/execute.rs`; 0 in savepoint/statement-rollback code. Whether deltas from a failed statement inside an explicit transaction are discarded: UNVERIFIED.
- Crash mid-commit: view and state btrees are ordinary pages in the same WAL transaction (inferred from `commit_txn` ordering; UNVERIFIED by test).

## 4. Operators and limitations (HEAD)

| Operator | Status | Implementation | Lines |
|---|---|---|---|
| Filter (WHERE) | yes; complex predicates get a projection inserted first | `FilterOperator` | `compiler.rs:~985`; unsupported filter ops error at `compiler.rs:2169, 2231, 2281` |
| Project | yes | `ProjectOperator` + `expr_compiler.rs` | `compiler.rs:950-990` |
| Aggregate / GROUP BY | COUNT(*), COUNT/SUM/AVG (incl. DISTINCT), MIN, MAX; argument must be a plain column reference | `AggregateOperator` | `aggregate_operator.rs:101-110`; errors `compiler.rs:~1243, 1260, 1275, 1281` |
| DISTINCT | yes, as `AggregateOperator` with group-by all columns and empty aggregate list | `compiler.rs:~1425-1457` |
| Join | INNER equi-join only; ON must be column = column; at least one equality | `JoinOperator` | `compiler.rs:1332-1370`; `join_operator.rs:380-401` rejects LEFT/RIGHT/FULL/CROSS with "not yet supported in incremental views" |
| Join delta rule | `dR ⋈ dS` plus stored-state lookups for `dR ⋈ S`, `R ⋈ dS` | `join_operator.rs:480-545` ("Component 3" comment) |
| UNION / UNION ALL | yes (two-input merge; ALL mixes source table name into hash) | `MergeOperator` | `merge_operator.rs:18-26`; `compile_union` |
| Sort / ORDER BY, LIMIT, VALUES, EmptyRelation, WITH/CTE, CTERef | rejected: "Unsupported operator in DBSP compiler" | - | `compiler.rs:1459-1470` |
| Recursion | no (WithCTE/CTERef rejected; `DbspOperator::Merge` doc mentions recursive CTEs, compiler does not reach it) | - | `compiler.rs:318, 1459-1470` |

Other listed limitations:

| Limitation | Source |
|---|---|
| Not all SQL functions supported in definitions | `create-materialized-view.mdx`, `cli/manuals/materialized-views.md` |
| MV cannot reference another view / MV | same |
| `TEMPORARY` not supported | `create-materialized-view.mdx` |
| Not on attached databases | `translate/view.rs:32-36` |
| No cross-database references | `util.rs:1868` `validate_select_for_views` |
| `ALTER TABLE` blocked on tables with dependent MVs | `translate/alter.rs:902-911` |
| WITHOUT ROWID base tables rejected | `COMPAT.md:155`; `execute.rs:~12758` |
| Circuit format versioned into state table name (`_v1_`); `has_compatible_dbsp_state_table` checks | `schema.rs:1121-1125`, `compiler.rs:397` |
| Operator types marked "needs to be audited for thread safety" (issue #1552), `unsafe impl Send/Sync` | `view.rs:142-146`, `compiler.rs:~350, ~425` |
| Join output identity by value hash (duplicates with equal values share rowid, carried by weight) | `join_operator.rs:~512-516` |

## 5. Transfer to a stock-SQLite loadable extension

### Techniques with no engine ownership required

| Technique in Turso | Where | Stock-SQLite analogue |
|---|---|---|
| Single state table per view: `(operator_id, zset_id, element_id, value, weight)` with composite PK; operator/column/kind packed into one integer (`id<<16 | col<<2 | kind`) | `translate/view.rs:201-212`; `operator.rs:64` | ordinary table (or WITHOUT ROWID table) created by the extension; same packing |
| View output table stores `weight` as trailing column; row key = 64-bit hash of row values | `compiler.rs:625-640` | ordinary table; hash via app-defined function |
| Ordered delta list (update = delete-then-insert, order preserved, consolidate on demand) | `dbsp.rs:202-260` | staging table with sequence column |
| Per-operator `eval` (no side effects) vs `commit` (persist state) split | `operator.rs:205-224` | read-own-writes query path vs xSync-time maintenance |
| Uncommitted-overlay read: compute delta through circuit on read, merge Z-set onto persisted rows | `cursor.rs:47-122` | virtual table xFilter unions persisted table with delta computed from staging |
| MIN/MAX state kept as ordered keys in the state btree ("BTree ordering gives both") | `aggregate_operator.rs:79` | index on state table; `ORDER BY ... LIMIT 1` |
| Initial population via ordinary SQL SELECT per source table fed into the circuit | `view.rs:690, 1147-1260` | same, via `sqlite3_prepare` inside the extension |
| Schema guard: block ALTER on sources with dependents | `translate/alter.rs:902` | `sqlite3_set_authorizer` on `SQLITE_ALTER_TABLE` (UNVERIFIED fit) |
| State-format version in internal table name | `schema.rs:135`, `compiler.rs:397` | same naming |

### Mechanisms that depend on owning the engine

| Mechanism | Where | Reason |
|---|---|---|
| Delta capture in `Insert`/`Delete` opcodes with old-record read from the btree cursor before write | `execute.rs:12502-12960` | stock SQLite exposes this only via triggers, `sqlite3_preupdate_hook` (requires `SQLITE_ENABLE_PREUPDATE_HOOK`), session extension, or vtab writes |
| `apply_view_deltas` invoked from `commit_txn` before pager commit, with async I/O resume | `vdbe/mod.rs:2677-2801` | stock SQLite `sqlite3_commit_hook` cannot run SQL on the same connection; the user's xSync collector is the substitute |
| Rollback = drop in-memory deltas, since nothing was written pre-commit | `vdbe/mod.rs:2692-2696` | requires the commit-path hook above |
| `CREATE MATERIALIZED VIEW` grammar, `sqlite_schema` row of type `view` with a rootpage, `Insn::PopulateMaterializedViews`, `Insn::ParseSchema` | `translate/view.rs:57-278` | parser/schema ownership; extension equivalent is a vtab module `CREATE VIRTUAL TABLE ... USING` |
| `MaterializedViewCursor` as a native cursor type chosen in `op_open_read` | `execute.rs:~1436` | extension equivalent is a vtab cursor |
| Logical planner reuse (`translate/logical.rs` `LogicalPlanBuilder`) to compile SELECT into the circuit | `view.rs:~238-262` | stock SQLite has no public logical-plan API; extension parses SQL itself or uses `EXPLAIN` bytecode |
| Nested statement accounting (`conn.start_nested/end_nested`) during population | `view.rs:~1163-1166` | internal connection state |

## 6. License

MIT, "Copyright 2024 the Turso authors" (`LICENSE.md:1-3`). `NOTICE.md` lists
third-party dependencies. The `core/incremental` code has no separate header.

## Sources

- https://github.com/tursodatabase/turso (clone at bec3bbff)
- https://turso.tech/blog/introducing-real-time-data-with-materialized-views-in-turso
- https://docs.turso.tech/sql-reference/statements/create-materialized-view
- https://github.com/tursodatabase/turso/pull/8015
- https://github.com/tursodatabase/turso/issues/9325
- https://github.com/tursodatabase/turso/issues/2350
