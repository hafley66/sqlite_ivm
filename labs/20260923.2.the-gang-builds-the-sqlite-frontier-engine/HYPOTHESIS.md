# HYPOTHESIS — an independent SQLite extension engine lane

One reusable SQLite-backed incremental frontier engine as a library crate
(`frontier-engine`) plus a loadable extension cdylib (`frontier-ext`), inside
lab `labs/20260923.2.the-gang-builds-the-sqlite-frontier-engine/`. It must
reproduce both oracle TSV pairs from `../../plans/engine-iso/` and leave
measurements behind.

## 1. Public type signatures (the contract, first)

```rust
pub enum Cell { Null, Integer(i64), Real(f64), Text(String), Blob(Vec<u8>) }
impl rusqlite::ToSql for Cell { /* storage class preserved exactly */ }

pub struct Tuple(pub Vec<Cell>);

pub enum Sign { Insert, Delete }              // engine's signed row semantics

pub struct SourceChange { pub relation: String, pub sign: Sign, pub row: Vec<Cell> }
impl SourceChange {
    pub fn insert(relation: impl Into<String>, row: impl IntoIterator<Item = Cell>) -> Self;
    pub fn delete(relation: impl Into<String>, row: impl IntoIterator<Item = Cell>) -> Self;
}

pub struct OutputColumn { pub name: String }

pub struct OutputChange { pub sign: Sign, pub row: Vec<Cell> }   // net signed output row

pub struct Program { /* Arc<Installed>; owns no connection */ }
impl Program {
    pub fn install(conn: &Connection, name: &str, select_sql: &str) -> Result<Self, EngineError>;
    pub fn open(conn: &Connection, name: &str) -> Result<Self, EngineError>;
    pub fn name(&self) -> &str;
    pub fn output_columns(&self) -> &[OutputColumn];
    pub fn sources(&self) -> &[String];
}

pub trait Frontier {
    fn settle(&self, conn: &Connection, batch: &[SourceChange])
        -> Result<Vec<OutputChange>, EngineError>;
    fn snapshot(&self, conn: &Connection) -> Result<Vec<Tuple>, EngineError>;
    fn frontier_id(&self, conn: &Connection) -> Result<u64, EngineError>;
    fn teardown(&self, conn: &Connection) -> Result<(), EngineError>;
}
impl Frontier for Program {}   // same object shape for union and grouped COUNT/SUM

pub struct EngineError { /* stage, relation, kind */ }
pub enum ErrorKind {
    Unsupported(&'static str), UnknownRelation, UnknownColumn,
    Arity { /* relation, expected, got */ }, State(String), Sqlite(String),
}
impl fmt::Display for EngineError { /* "[stage/relation] kind" */ }
impl From<EngineError> for rusqlite::Error { /* Error::ModuleError(err.to_string()) */
}
```

Extension surface (`crates/frontier-ext`, entry `sqlite3_frontier_ext_init`):

```rust
frontier_install(name TEXT, select_sql TEXT) -> INTEGER  -- installs, returns 1
frontier_drop(name TEXT) -> INTEGER                      -- tears down, returns 1
-- both SQLITE_UTF8 | SQLITE_DIRECTONLY
```

Both programs — the union `access` case and the grouped `team_cost`
COUNT/SUM case — are installed and driven through this one API shape. No
plugin-specific second API, no Sprefa rule names or term-arena types.

## 2. Lifetimes

- `Program` is `Arc<Installed>` and owns **no** connection. All state lives in
  the connection's schema and catalog, so a second handle (`Program::open`)
  or the extension path can drive the same program without borrow coupling.
- Every `Frontier` method takes `&Connection` explicitly; nothing outlives the
  call except committed rows.
- `sqlite_ext::watch(conn, name, tables, trigger)` moves the boxed trigger
  into the collector; it is taken out only during `on_batch` (re-entrant
  flush is rejected), so the trigger never dangles even when a settle errors.
- The extension registry (`LazyLock<Mutex<HashMap<String, Program>>>`) holds
  only the handle `frontier_drop` needs; dropping the connection drops
  everything else with the schema.

## 3. Storage layout and read/write sequence

Objects, all named `frontier_{program}_*` (catalog tables shared, named
`frontier_catalog`, `frontier_catalog_column`):

- `frontier_{p}_stage` — per-frontier staging of the raw batch (`__n` sequence,
  relation name, `__sign`, one untyped column per source column).
- `frontier_{p}_s{i}` — netted per-scan stagings (`__weight` signed multiplicity
  per distinct row).
- `frontier_{p}_j{i}` — per-join intermediates for two-term propagation.
- `frontier_{p}_root` — the maintained result keyed by the group-by/union key
  (`__weight` union support, `__n`/`__s{k}` group state).
- `frontier_{p}_delta` — the last settled frontier's net signed output change.
- `frontier_{p}_x{i}`, `frontier_{p}_rootk` — join-key indexes on sources and
  the unique root key index.
- `frontier_{p}` — the public view over `root`; triggers per watched table feed
  the collector vtab.
- Bookkeeping columns (`__n __s __weight __mult __sign __bw __bn __bs`) are
  declared `INTEGER NOT NULL`; **value columns carry no declared type** so a
  cell's storage class survives staging untouched.

Install (`SAVEPOINT frontier_sp_install`): DDL, catalog rows (program SQL text,
frontier counter, output schema — real rows, not JSON), then the collector
watch. `RELEASE` or full rollback.

Settle of one frontier, in order: validate batch (relation known, arity exact,
NULL cells rejected as `Unsupported`) → clear stage/stagings/joins → insert
staged rows (`params_from_iter` over `Cell`) → per-scan netted fills → join
fills → root touch / upsert / delta maintenance → frontier bump → snapshot +
delta readback → `tracing::debug! frontier_settled` (statements, sql_bytes).

The join equation over netted stagings, three UNION ALL terms — cross-term
counted once, duplicates multiply, self-joins safe:

```
W = δL ⋈ R_live  +  L_live ⋈ δR  −  δL ⋈ δR
```

Group root delta: `touch` computes −old from live root rows, the upsert applies
+new, and the delta table records rows whose visible form changed.

## 4. Extension boundary crossings

1. **SQL → scalar fn.** `SELECT frontier_install('access', '…')` crosses into
   Rust; `unsafe { ctx.get_connection()? }` recovers the host connection.
   `SQLITE_DIRECTONLY` because both functions mutate schema.
2. **Install** registers the engine's vtab module and watch collector on the
   host connection (same `sqlite3*`, no second registration).
3. **Source writes → collector.** Ordinary DML on watched tables fires
   `AFTER INSERT/DELETE` triggers writing change rows into the collector vtab.
   At `xSync` the collector hands one batch to the engine's trigger, which
   settles **inside the committing transaction** (no savepoint — see §8) and
   converts engine errors to `rusqlite::Error` so a failing settle fails the
   `COMMIT`. SQLite then rolls the whole transaction back by itself:
   the extension test observes `is_autocommit() == true` immediately after the
   failed `COMMIT`, with the previous snapshot readable.
4. **Engine → SQL.** Results surface as an ordinary view (`frontier_access`) and
   delta table, readable in pure SQL by any host.

Host/extension geometry: the cdylib builds against `rusqlite`
`loadable_extension` (system libsqlite3), the test host against `bundled` —
the same geometry sqlite-ext's own fixture tests validate. Two SQLite copies
in one process is the known cost; all cross-boundary state flows through the
host connection's schema, never through Rust globals.

## 5. Explicit `Unsupported` matrix

Rejected at compile/install with `ErrorKind::Unsupported` (10 shapes asserted
in tests): `WHERE`, `DISTINCT`, `HAVING`, `ORDER BY`, `LIMIT`, outer/cross and
comma joins, `AVG`, `MIN`/`MAX`, `count(col)`. At settle: unknown relation,
wrong arity, `NULL` cells (staging is the place to refuse — there is no
migration path for a NULL join key). No fallback recomputation anywhere: a
program either increments or refuses.

## 6. Measurements (dev profile, in-memory setup, file DB for sizes)

`cargo run --offline --example measure` prints the full report. The run below
covers 10 access frontiers + 5 team_cost frontiers (7 packet frontiers, 3
guardrail repeats, 5 aggregate frontiers):

- Prepares: 259 cached, 27 fresh. The 59 fresh-labeled statement events and
  75 "other" events (1,594 vm steps) are install-time DDL batches and the
  per-settle delta readback (`query_map` prepares fresh by sqlite-ext's API —
  no cached map variant exists).
- PROFILE: 393 statements, 12,753 vm steps total; 9,780 on cached settle
  statements.
- Engine settles: access — 10 frontiers, 196 statements, 29,214 sql bytes;
  team_cost — 5 frontiers, 72 statements, 9,816 sql bytes.
- Objects at steady state: 19 tables, 4 indexes, 12 triggers, 2 views = 37
  `frontier_%` rows in `sqlite_master` (two programs installed).
- Memory: SQLite allocator current 377,968 B, peak 495,984 B; connection
  statements 106,848 B, cache 153,600 B, schema 32,160 B.
- Process: peak RSS 6,914,048 B (~6.6 MiB), CPU user 0.053 s + system 0.036 s.
  An independent rerun peaked at 8,404,992 B; statement and allocator counts
  matched the recorded run.
- Files: db 4,096 B; WAL 902,312 B before `PRAGMA wal_checkpoint(TRUNCATE)`,
  0 B after (WAL retains every settled frame until checkpointed).
- **Repeat guardrail** (same-shape settles): vm steps A=490, B=490 —
  deterministic; 18 cached prepare calls per settle, 0 new statements; fresh
  prepares grow only by the delta readback (+2 across the repeats), matching
  the contract in §3.

## 7. Exact commands

```sh
cd labs/20260923.2.the-gang-builds-the-sqlite-frontier-engine
sqlite3 -separator "$(printf '\t')" :memory: < ../../plans/engine-iso/2_oracle.sql \
  | diff - ../../plans/engine-iso/3_expected.tsv                 # PRIMARY_ORACLE_OK
sqlite3 -separator "$(printf '\t')" :memory: < ../../plans/engine-iso/3b_aggregate.sql \
  | diff - ../../plans/engine-iso/3c_aggregate_expected.tsv      # AGGREGATE_ORACLE_OK
cargo fmt --all -- --check
cargo test --offline                             # 6 case tests + 1 extension + 1 doc test
cargo test --offline --manifest-path crates/frontier-ext/Cargo.toml
cargo run --offline --example cases              # 15-frontier walkthrough, self-asserting
cargo run --offline --example measure            # this report
```

All green on the recorded run: both oracle diffs exit 0, `fmt --check` clean,
8 tests pass (6 `cases`, 1 `extension`, 1 doc), both examples exit 0.

## 8. Failures hit and what they taught

- **Savepoint inside the collector's xSync fails** ("cannot open savepoint -
  SQL statements in progress"). Settlement that runs inside a committing
  transaction must not open a `SAVEPOINT`; the failing `COMMIT` is already
  the atomicity boundary. Direct `Frontier::settle` calls (outside COMMIT)
  keep the savepoint wrapper. This is why settle has an explicit in-commit
  mode rather than one unconditional wrapper.
- **SQLite widens integer overflow to REAL silently in expressions.** Two
  `i64::MAX` costs net to one staged row whose `2*cost` product becomes a
  float; only `sum()` over integer inputs raises "integer overflow". The
  callback-failure test therefore uses two distinct near-max costs
  (MAX, MAX−1) so the weighted products stay integers and `sum()` itself
  overflows, failing the collector and the `COMMIT` with
  `[settle/team_cost] sqlite: touch: integer overflow`.
- **The failed COMMIT needs no manual ROLLBACK** — SQLite rolls back on its
  own (the test asserts `is_autocommit()` and that sources are unchanged);
  a following explicit `ROLLBACK` errors "no transaction is active".
- **`UserFunctionError` is gated behind rusqlite `functions`**, which the
  non-loadable build does not enable — engine errors cross as
  `Error::ModuleError(text)` instead.
- **Engine deltas for SQL-settled frontiers are consumed by the collector**;
  driving `settle()` directly over the same changes double-settles (net-zero)
  — the tests keep the two paths strictly separate.
- Catalog rows must be real rows inserted during install; omitting them made
  `Program::open` unable to recompile the program.

## 9. Revisions and environment

- sqlite_ivm worktree `.boop-worktrees/feature/iso-sqlite-frontier` HEAD:
  `51d16a5` (this lab commits on top).
- hafley-rs (sqlite-ext, hafley-observe) path dep:
  `/Users/chrishafley/projects/hafley-rs-wt/main-codex-attribution/crates/`,
  rev `91b41b47220cb1c13b47688936694dc7a22b5f90`.
- rusqlite `=0.40.2` everywhere; bundled+`load_extension` only in test
  dev-deps; `loadable_extension` only in the cdylib workspace (the two
  feature sets cannot share a build graph — hence `exclude` in the lab
  workspace and frontier-ext's own `[workspace]`).
- sqlite3 CLI 3.43.2 (oracle runs).
- macOS 14.6 (Darwin 23.6.0), arm64, Apple M2 Pro.
