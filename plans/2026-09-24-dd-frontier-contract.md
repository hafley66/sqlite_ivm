# DD packet vs promoted frontier engine: public-contract map (2026-09-24)

Lane: `feature/frontier-dd-contract`. Scope: map `labs/20260923.3.dd-inside-sqlite`
(the DD packet + SQLite extension) onto `crates/frontier-engine`'s public contract,
with an executable shared-case checklist for a future generic DD backend. No engine
or extension source was changed; evidence is from source inspection and the runs
logged in §9.

## 1. Exact public signatures

### 1.1 Promoted engine: `crates/frontier-engine` (`frontier_engine`)

```rust
pub enum Cell { Null, Integer(i64), Real(f64), Text(String), Blob(Vec<u8>) }   // storage class preserved end-to-end
pub struct Tuple(pub Vec<Cell>);                                               // declared column order
pub enum Sign { Insert, Delete }        // Sign::as_integer() -> 1 | -1
pub struct SourceChange { pub relation: String, pub sign: Sign, pub row: Vec<Cell> }
                                        // full row image, declared column order
pub struct OutputColumn { pub name: String }
pub struct OutputChange { pub sign: Sign, pub row: Vec<Cell> }                 // emitted only when the visible form changed

pub struct Program { inner: Arc<catalog::Installed> }                          // handle owns no connection
impl Program {
    pub fn install(conn: &Connection, name: &str, select_sql: &str) -> Result<Self, EngineError>;
    pub fn open(conn: &Connection, name: &str) -> Result<Self, EngineError>;   // reload from catalog row
    pub fn name(&self) -> &str;
    pub fn output_columns(&self) -> &[OutputColumn];
    pub fn sources(&self) -> &[String];
}
pub trait Frontier {
    fn settle(&self, conn: &Connection, batch: &[SourceChange]) -> Result<Vec<OutputChange>, EngineError>;
    fn snapshot(&self, conn: &Connection) -> Result<Vec<Tuple>, EngineError>;
    fn frontier_id(&self, conn: &Connection) -> Result<u64, EngineError>;      // monotone; unchanged by rolled-back frontiers
    fn teardown(&self, conn: &Connection) -> Result<(), EngineError>;
}
pub enum Stage { Parse, Plan, Install, Teardown, Collect, Settle, Read }
pub enum ErrorKind { Unsupported(&'static str), UnknownRelation(String), UnknownColumn(String),
                     Arity { relation: String, expected: usize, got: usize }, State(String), Sqlite(String) }
pub struct EngineError { pub stage: Stage, pub relation: String, pub kind: ErrorKind }
```

Call sites: `src/3_extension.rs:71-97` wraps `Program::install/open/teardown` as
`sqlite_ivm_frontier_install/drop`; `engine.rs:49-72` (`ProgramTrigger`) adapts
`sqlite_ext::RowChange` batches into `SourceChange` and calls `settle(..., in_commit = true)`.

Settle pipeline (`engine.rs:88-224`, inside the caller's transaction; savepoint-wrapped
when `in_commit == false`): clear staging (stage, scan deltas, join deltas, touch, delta)
→ stage the batch row-by-row (`__seq` = batch position) → per scan, net the staging
(`SUM(__sign)` grouped over needed columns, zero nets dropped) → per join, the three-term
equation `W = dL ⋈ R + L ⋈ dR − dL ⋈ dR` (cross-term exactly once) → root: capture
before-images of touched rows, apply summed weight/aggregate deltas, drop invisible rows,
emit the net delta → bump `frontier_catalog.frontier`, read `frontier_{p}_delta` in output
order. An error at any step rolls back to the savepoint (or fails the COMMIT on the
collector path), leaving the previous committed state readable.

SQLite objects per program `p` (`catalog.rs`): `frontier_catalog(name, sql, frontier,
install)`, `frontier_catalog_column(name, pos, col)`, `frontier_{p}_root`, 
`frontier_{p}_delta(__sign, …)`, `frontier_{p}_touch`, one stage table per scan, one
delta table per join, view `frontier_{p}`, source join indexes, and the collector vtab
plus `{collector}_{table}_{ins|upd|del}` triggers over the sources.

Supported plan grammar (`plan.rs:1-12`): `SELECT keys, count(*), sum(col) FROM t GROUP
BY keys` (aggregate root); inner equi-joins over source tables; any number of such
branches under set `UNION` (union root, support-counted). Everything else is an explicit
`ErrorKind::Unsupported`; `Cell::Null` in a batch is unsupported (`engine.rs:246-252`).

### 1.2 DD packet: `labs/20260923.3.dd-inside-sqlite` (`frontier-dd-packet`)

```rust
pub enum Shape { Access, Group }                       // the two packet graphs, hardcoded
pub struct Change { pub table: usize, pub id: i64, pub a: i64, pub b: i64, pub weight: isize }
pub type Delta = (Vec<i64>, isize);                    // output row + arbitrary isize weight
pub struct Engine { sender: mpsc::Sender<Command>, thread: Option<JoinHandle<()>> }
impl Engine {
    pub fn new(shape: Shape) -> Self;                  // spawns the timely worker, waits for ready
    pub fn apply(&self, changes: Vec<Change>) -> Result<Vec<Delta>, String>;
    pub fn snapshot(&self) -> Result<Vec<Vec<i64>>, String>;   // multiset: repeats rows by weight
}
impl Drop for Engine  // sends Stop, joins the thread
```

Worker (`0_lib.rs:78-156`): one `timely::execute_directly` thread, four
`InputSession<u64, [i64; 3], isize>` inputs (fixed table order: 0 membership,
1 permission, 2 direct_grant, 3 job; fixed `[id, a, b]` arity). Graphs:

- `Shape::Access`: `membership.map(|[_,p,t]| (t,p)) ⋈ permission.map(|[_,t,r]| (t,r))
  → [p,r]`, concat `direct → [p,r]`, `.distinct()`.
- `Shape::Group`: `job.map(|[_,t,c]| (t,c)).explode(|(t,c)| Some((t,(1,c)))).count_total()
  → [t, count, sum]`.

Per `Apply`: feed every change as `(row, weight)`, `epoch += 1`, `advance_to(epoch)` +
`flush()` on all inputs, `while probe.less_than(&epoch) { worker.step() }`, then drain the
`consolidate().inspect` accumulator (`BTreeMap<Vec<i64>, isize>`) into the reply — skipping
zero weights — while merging it into the snapshot map (removal at net zero). One batch =
one epoch = one settled frontier; all state is process memory; SQLite is not involved.

### 1.3 DD extension: `labs/20260923.3.dd-inside-sqlite/ext` (`frontier-dd-ext`)

SQL surface: `SELECT dd_frontier_install('access'|'team_cost')`,
`dd_frontier_drop(name)`. Objects per case: result table `dd_frontier_{access|
team_cost}` with declared keys (`PRIMARY KEY(person,resource)` / `team INTEGER PRIMARY
KEY`), a parallel `dd_frontier_{name}_delta(__sign, …)`, and a row in
`dd_frontier_catalog(name TEXT PRIMARY KEY, generation INTEGER)`.

Intake: `sqlite_ext::watch(db, "dd_watch_{name}", tables, Maintain { shape, engine,
generation })` — the shared collector delivers one `&[RowChange]` per transaction at
`xSync`, before SQLite's pager commit (`ext/0_lib.rs:142-182`).

`Maintain::on_batch`: clear the `_delta` table; if `dd_frontier_catalog.generation ==
self.generation`, send the batch (mapped by fixed table name → index 0-3, three integer
columns enforced, `weight = sign.as_integer()`) to the persistent worker and write its
net deltas — **negatives before positives** so a group can replace its row under the
unique key; else rebuild: new `Engine::new(shape)`, `apply(source_rows(db, shape))` over
the rows visible in this batch, then write the visible diff (current table vs worker
snapshot) as −1/＋1 deltas. Finish by `generation += 1` in the same transaction.

### 1.4 Shared intake: `sqlite_ext` (hafley-rs, revision in `Cargo.toml` paths)

```rust
pub enum Sign { Insert, Delete }                       // UPDATE = Delete(old image) + Insert(new image)
pub struct RowChange { pub table: String, pub sign: Sign, pub values: Vec<Value>, pub sequence: u64 }
pub trait BulkTrigger { fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> Result<()>; }
pub fn watch(db: &Connection, name: &str, tables: &[&str], trigger: impl BulkTrigger) -> Result<()>;
```

One call per transaction at `xSync`, changes in sequence order, empty batches skipped,
memory staging with spill (`STAGED_ROWS = 10_000`, `STAGED_BYTES = 8 MiB`), savepoint
marks so `ROLLBACK TO` unwinds staged rows.

## 2. Concrete shared batch, exact rows and signs (measured, both engines)

Probe: throwaway crate `/tmp/dd-contract-probe` driving the real `frontier_dd_packet::Engine`
and the real `frontier_engine::Program` (collector path: source SQL in one transaction per
frontier) over identical batches. Output verbatim; both engines agree row-for-row.

Access frontier `SELECT person, resource FROM direct_grant UNION SELECT m.person,
p.resource FROM membership m JOIN permission p ON p.team = m.team`:

| step | change (table, id, image, sign) |
|---|---|
| seed | `membership(1; 1,10)+`, `permission(1; 10,100)+`, `permission(2; 10,200)+`, `direct_grant(1; 9,100)+` |
| batch | `membership(1; 1,10)−` (delete), `membership(2; 2,10)+`, `direct_grant(2; 1,300)+`, `direct_grant(3; 5,5)+` **and** `direct_grant(3; 5,5)−` (nets to zero inside the batch) |

- DD seed delta: `(+1)[1,100] (+1)[1,200] (+1)[9,100]` — engine `frontier_access_delta`:
  `[1,1,100] [1,1,200] [1,9,100]` (`__sign` first). Snapshot after seed:
  `{(1,100),(1,200),(9,100)}` on both.
- Batch delta, both engines: `(+1)[1,300] (+1)[2,100] (+1)[2,200] (−1)[1,100] (−1)[1,200]`.
  `(9,100)` stays (support unchanged); the net-zero pair produces nothing. This batch
  deletes a join support row at fanout 2 — the cross-term case: `(1,100)` and `(1,200)`
  each lose their only derivation and retract together.
- Snapshot after batch, both: `{(1,300),(2,100),(2,200),(9,100)}`.

Group frontier `SELECT team, count(*) jobs, sum(cost) total_cost FROM job GROUP BY team`:

| step | change |
|---|---|
| seed | `job(1; 10,5)+`, `job(2; 10,7)+`, `job(3; 20,1)+` |
| batch | `job(2; 10,7)−` (old image), `job(2; 10,8)+` (new image — the UPDATE encoding), `job(3; 20,1)−` |

- Seed delta, both: `(+1)[10,2,12] (+1)[20,1,1]`.
- Batch delta, both: `(+1)[10,2,13] (−1)[10,2,12] (−1)[20,1,1]` — team 10's row is
  replaced under its unique key (count stays 2, sum 12→13), team 20 vanishes. The ext
  orders the negative write before the positive for exactly this case; the engine's root
  upsert/delete needs no ordering.
- Snapshot after batch, both: `{(10,2,13)}`.

Contract hazard found by the probe (first run, since corrected): calling
`program.settle(&conn, batch)` for changes that were **also** committed through SQL on
the same connection double-applies them — the collector settles the SQL at COMMIT
(`frontier_id` reached 10 after two logical frontiers) and `settle` re-stages the same
rows (group seed produced `count=4, sum=24`). `Frontier::settle`'s doc contract is that
the batch mirrors changes that have already landed (`tests/cases.rs:287-288`), but
`Program::install` always registers the collector (`catalog.rs:207`), so on a
collector-watched connection the only safe intakes today are collector-only (bench arms)
or `settle`-only batches that never pass through SQL (net-zero/empty/error batches in
`tests/cases.rs`). A generic backend needs one arbitration rule; see §7.

Rollback/reopen, measured on the engine path: `BEGIN; INSERT direct_grant(4,7,700);
ROLLBACK;` → `frontier_access_delta` unchanged, `frontier_id` still `Ok(2)` (bumped only
by settled frontiers), snapshot unchanged. `Program::open(&conn, "access")` after the
fact recompiles from the catalog row and returns the identical snapshot. The DD extension
cannot do this atomically: its worker advanced at `xSync`, so a later SQLite rollback
leaves it ahead — `tests/0_extension.rs:45-111` forces the generation gap (resets
`generation` to 0, commits a second batch) and asserts the rebuild: access
`{[1,100],[1,200],[2,100],[2,200]}`, team_cost `[10,2,13]`, delta
`[1,1,200] [1,2,100] [1,2,200]`; a following `INSERT` + `ROLLBACK` changes nothing.

## 3. Comparison matrix

| aspect | DD Rust (`frontier-dd-packet`) | DD extension (`frontier-dd-ext`) | ISO Rust (`lab_20260923_1`) | main sqlite_ivm frontier (`frontier_engine` via `sqlite_ivm_frontier_*`) | legacy plugin (`USING sqlite_ivm`) |
|---|---|---|---|---|---|
| program representation | `Shape::{Access,Group}` enum; two hardcoded graphs | same two cases, selected by name string | `Program { outputs: Vec<OutputDecl> }` over `PlanNode` tree (Scan/Union/Join/Aggregate/Project; Difference = explicit Unsupported) | SQL SELECT subset compiled to `Compiled { root: Union|Group, scans, joins }` | SQL SELECT compiled to vtab plan; virtual table `result` |
| cell domain | `[i64; 3]` fixed arity, fixed 4-table order | three integer columns enforced, table → 0-3 | `Cell = i64`, `Row = Vec<i64>` | `Cell` 5 storage classes, declared arity | SQLite values as stored |
| intake | `Engine::apply(Vec<Change>)` from Rust | collector `on_batch` at xSync | `FrontierEngine::apply(program, Frontier)` | collector at xSync, or direct `Frontier::settle` (see §2 hazard) | SQL writes; triggers on sources (`src/1a_relational.rs:338`) + vtab collector forwarding (`src/2_vtab.rs:786-790`) |
| batch/frontier semantics | epoch += 1, advance+flush, probe drain; reply = net deltas | one `on_batch` per transaction, worker epoch advanced inside | one `Frontier { id: String, changes }`; consolidation is the engine's job | one settle: stage → net → join equation → root → bump | per-transaction maintenance; no frontier id exposed |
| output delta | `(Vec<i64>, isize)` general weights | `__sign` ±1 rows only; non-±1 rejected ("outside set semantics") | `OutputDelta { output, changes: Vec<OutputChange{sign,row}> }` per output | `Vec<OutputChange>`; persisted in `frontier_{p}_delta(__sign, …)`, cleared at each settle start | none exposed (bench checks snapshots only for this arm) |
| snapshot | in-memory map, multiset expansion | `dd_frontier_{name}` tables (keyed) | sorted `Vec<Row>` per output | `frontier_{p}_root` via SELECT, deterministic order | the view itself |
| state location | process memory only (timely arrangements + maps) | split: worker memory + SQLite result/delta/catalog tables | process memory (arenas, arrangements) | entirely SQLite (staging, deltas, root, catalog); handle is stateless `Arc` | SQLite shadow tables |
| rollback | not applicable (no SQLite coupling) | generation-gap rebuild from visible source rows (measured §2) | frontier unwound on Err | atomic: settle runs in the committing transaction; `frontier_id` unchanged on rollback (measured §2) | maintenance runs in-transaction; SQLite rollback covers it |
| reopen | new `Engine` + full re-apply | rebuild on generation mismatch | new `Engine` + re-install | `Program::open` recompiles from `frontier_catalog` row (measured §2) | `xConnect` rebinds the persisted virtual table |
| teardown | `Drop` → Stop + join | `dd_frontier_drop` drops tables + catalog row; watcher vtab drop drops the trigger struct (worker `Drop`ped) | `FrontierEngine::uninstall` | `Frontier::teardown` drops every object + collector, sources untouched | `sqlite_ivm_drop` drops the vtab |
| errors | `Result<_, String>` | `rusqlite::Error::ModuleError(String)` | `EngineError { stage, kind, relation, output }`, `ErrorKind::Unsupported { shape }` | `EngineError { stage, relation, kind }`, 7 stages | SQLite vtab errors |
| multi-output / generality | one graph per `Engine` | one case per install | many named outputs per program | one output per program; N programs per connection | one view per vtab |

## 4. Where the DD packet already meets the promoted contract

For the two packet shapes, the measured behavior is identical: same net output deltas,
same snapshots, same set semantics, same batch netting (including an intra-batch
net-zero pair), same group-row replacement. The DD worker's epoch + probe-drain loop is
the same "apply one complete frontier, return its net output change" contract as
`Frontier::settle`, and `sqlite_ext::BulkTrigger` is a proven common intake seam — both
the promoted engine (`ProgramTrigger`) and the DD extension (`Maintain`) already consume
it unchanged.

## 5. Gaps vs a generic relational Program/Frontier contract

1. **Program is an enum, not a plan.** `Shape::{Access,Group}` fixes graph, table set,
   column positions, and arity. A generic backend needs ISO's `PlanNode`-style
   declarative plan (or the engine's compiled subset) with named relations and declared
   arities.
2. **Two intake paths, no arbitration.** Collector settle at COMMIT and direct
   `Frontier::settle` coexist on one connection with no guard; mixing double-applies
   (§2, measured). The engine's own tests dodge the overlap; a generic contract must
   pick one intake or make `settle` reject collector-covered changes.
3. **Identity representation.** DD keys changes by `(table index, id, [a,b])` positions;
   the engine requires full row images (delete carries the old image; UPDATE is
   delete+insert). A generic packet must fix one encoding; the engine's is the one
   already shared with `sqlite_ext::RowChange`.
4. **Weight domain.** DD speaks general `isize` (multiset); the extension refuses
   |w|≠1 outputs and the engine's persisted delta is ±1 only. ISO's union root is
   support-counted. Generic contract must state: multiset internally, set-visible, with
   an explicit refusal (not a silent clamp) for non-set outputs.
5. **No frontier id in DD.** `frontier_id` (monotone, rollback-stable, reopenable) has
   no DD equivalent; the ext approximates it with `dd_frontier_catalog.generation` plus
   the rebuild protocol.
6. **Rollback is a protocol for DD.** The SQLite engine keeps operator state in the
   source transaction. DD's worker can advance ahead of SQLite ⇒ generation compare + full rebuild + visible diff.
   A generic DD backend either persists operator state in SQLite or formalizes the
   rebuild as part of `settle`'s contract.
7. **Error contract.** `String` errors (DD) vs stage+relation+kind (engine/ISO). The
   promoted `Stage`/`ErrorKind` is the richer precedent and crosses the extension
   boundary inside the message text.
8. **Schema/storage-class coverage.** DD handles three-integer-column rows only; the
   engine preserves five storage classes end-to-end but rejects `NULL` in batches and
   unsupported plan shapes explicitly. A generic contract keeps the explicit-Unsupported
   rule; no silent recompute.

## 6. Executable shared-case checklist

Each item is checkable through the existing harness in `bench/frontier/0_stress.rs`
(one generated `Phase` stream + `Oracle` recompute + `expected_delta` set diff), plus
the §2 probe pattern. A future generic DD backend passes iff:

1. Every arm of the shared matrix (`direct-rust`, `direct-sqlite`, `rust-iso`,
   `sqlite-iso`, `sqlite-iso-ext`, `sqlite-ivm-frontier`, `dd-ext`, `sqlite-ivm`, `dd`)
   produces the identical snapshot at every checked step and the identical sorted
   signed delta (`expected_delta`) — the existing first test does this for 6 arms.
2. Intra-batch netting: an insert+delete pair of the same row in one frontier produces
   no delta and no snapshot change (§2 access batch; `tests/cases.rs:293-300`).
3. Join cross-term: deleting a support row at fanout > 1 retracts exactly the visible
   rows that lost all support, once each (§2 access batch).
4. Group replacement: cost update under a unique key yields `−old, +new` (plus group
   vanish rows), applied negative-before-positive in table form (§2 group batch;
   `tests/0_extension.rs:95-98`).
5. Multiset support: duplicate source rows keep support; 2→1 silent, 1→0 retraction
   (`tests/cases.rs:333-353`).
6. Atomicity: a source transaction that fails or rolls back leaves snapshot, delta
   table, and frontier id unchanged (§2 engine measurements; `callback_failure_fails_
   commit_keeps_previous_state`).
7. Reopen: a handle re-opened from the catalog returns the identical snapshot without
   re-seeding (§2 `Program::open`).
8. Recovery protocol: a host rollback after the settle hook leaves a detectable
   generation gap; the next batch detects it, rebuilds from visible source rows, and
   emits the visible diff (`tests/0_extension.rs:77-98`).
9. Explicit non-support: unsupported shapes, unknown relations, arity mismatches, and
   NULL cells return typed errors naming the stage/relation — never a success-shaped
   recompute (`unsupported_program_shapes_are_explicit`, §1.1 validation).
10. Empty frontier: valid, settles, bumps the frontier id exactly once
    (`tests/cases.rs:356-360`).

## 7. Adapter boundary the existing code already supports

A `Frontier`-shaped facade over `frontier_dd_packet::Engine` is mechanical today, and
only that:

- `SourceChange { relation, sign, row } → Change { table, id, a, b, weight }` — the
  mapping table (`membership→0 … job→3`) and the three-integer-column check already
  exist verbatim in `ext/0_lib.rs:44-68`; the reverse mapping (relation name from
  index, `Delta` weight → `Sign`) is the same table.
- `Vec<Delta> → Vec<OutputChange>` — weights are already consolidated per row by the
  worker; the ext's `write_delta` shows the required set-semantics clamp: refuse |w|≠1
  with "output weight … outside set semantics", write negatives first.
- Intake: implement `BulkTrigger` (the seam both current backends share); snapshot:
  forward to `Engine::snapshot`; teardown: `Drop`.
- Not mechanical, do not paper over: `frontier_id` (needs a host-side counter like
  `dd_frontier_catalog.generation`), and rollback (needs the §2/§5.6 rebuild protocol
  or persistent operator state). Both are host responsibilities in the current code and
  should stay explicit in any adapter.

Deliberately not proposed: no new DD implementation, no Sprefa concepts, no changes to
engine or extension source — this lane's only artifact is this file.

## 8. Bench arm coverage notes

- The stress binary's `--arms` set includes nine arms; the three `#[cfg(test)]` tests
  cover `direct-rust`, `direct-sqlite`, `rust-iso`, `sqlite-iso` (in-process
  `frontier_engine::Program`), `sqlite-ivm` (production plugin), and `dd` (Rust DD).
  `sqlite-iso-ext`, `dd-ext`, and `sqlite-ivm-frontier` need built cdylibs via
  `FRONTIER_EXT_PATH` / `FRONTIER_DD_EXT_PATH` (`scripts/15_frontier_stress.sh`) and are
  exercised only by running the binary, not by `cargo test`.
- The DD arm asserts both snapshot equality and the sorted signed delta against the
  recompute oracle on every checked step (`0_stress.rs:859-878`), including the join
  cross-term and group-churn cells.

## 9. Evidence log (exact commands and outcomes)

1. `cargo test --offline --manifest-path labs/20260923.3.dd-inside-sqlite/Cargo.toml`
   (worktree) — `frontier_dd_packet` lib: 0 tests; integration
   `extension_settles_and_recovers_from_generation_gap … ok` (35.60s; builds
   `libfrontier_dd_ext.dylib` itself). 1 passed, 0 failed.
2. `CARGO_TARGET_DIR="$PWD/target" cargo test --offline --manifest-path
   bench/Cargo.toml --bin frontier-stress` (worktree) — **failed before build**:
   `error: package collision in the lockfile: packages hafley-observe v0.1.2
   (/Users/chrishafley/projects/hafley-rs-wt/main-codex-attribution/crates/hafley-observe)
   and hafley-observe v0.1.2
   (/Users/chrishafley/projects/sqlite_ivm/.boop-worktrees/feature/hafley-rs-wt/main-codex-attribution/crates/hafley-observe)
   are different, but only one can be written to lockfile unambiguously` (exit 101).
   Cause: `crates/frontier-engine/Cargo.toml` and `labs/.../ext/Cargo.toml` pin absolute
   paths into `/Users/chrishafley/projects/hafley-rs-wt`, while the worktree's root and
   bench manifests resolve `../hafley-rs-wt` to the pinned sibling copy — two sources
   for one package version. Environmental; the pinned and canonical dep copies are
   src-identical (`diff -rq` clean). No repo file was modified for this.
3. Same command run from the canonical checkout `/Users/chrishafley/projects/sqlite_ivm`
   — verified same commit `73437ee` as this worktree, clean tree, private target dir
   `/tmp/dd-contract-target`: **3 passed, 0 failed** (0.61s):
   `every_arm_agrees_on_join_cross_term_and_group_churn`,
   `indexed_sqlite_batch_work_stays_flat_as_unrelated_state_grows`,
   `file_mode_reports_database_and_wal_pressure`.
4. Probe (throwaway, `/tmp/dd-contract-probe`, deps path into this worktree's lab +
   engine crates, canonical `sqlite-ext`): outputs quoted in §2. First run also produced
   the §2 double-apply measurements; corrected run produced the collector-path
   measurements. Probe code is throwaway; not committed.

No performance numbers are claimed beyond the wall-clock of the runs above; the lane
made no timing comparisons.

## 10. Missing tests / open items

- No test guards the settle/collector double-apply (§2 hazard): a case that installs a
  program, commits SQL writes, then calls `Frontier::settle` with the same batch and
  asserts a defined outcome (rejection or documented double-apply) is missing.
- `dd-ext` and `sqlite-iso-ext` arms are not covered by any `cargo test` target; they
  require the cdylib environment from `scripts/15_frontier_stress.sh`. A CI-runnable
  equivalent would close the gap (the DD lab's own integration test builds its cdylib
  in-process and could serve as the pattern).
- `labs/20260923.3.dd-inside-sqlite/HYPOTHESIS.md` itself notes the open case of a
  separate virtual table failing *after* the DD collector's `xSync`; `tests/
  0_extension.rs` covers the generation-gap path but not that ordering.
- Worktree lockfile collision (§9.2) will hit any lane running the bench workspace from
  a `.boop-worktrees` checkout; either the absolute paths in
  `crates/frontier-engine/Cargo.toml` need to become relative, or the runner must use a
  checkout where both forms resolve identically. Left untouched here (engine source
  freeze on this lane).
