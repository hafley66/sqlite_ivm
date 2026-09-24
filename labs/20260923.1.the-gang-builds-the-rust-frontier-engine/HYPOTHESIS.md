# HYPOTHESIS — an in-process Rust frontier engine, proven against the packet oracle

Lab `20260923.1.the-gang-builds-the-rust-frontier-engine` — one reusable
in-process Rust frontier engine installed with a program, fed signed source
frontiers, emitting signed output frontiers, with SQL-free snapshots, indexed
join maintenance, transactional rollback, and `hafley-observe` observability.
Everything below is what was built, what the oracle proves, and what remains
open.

Initial run: sqlite_ivm worktree `51d16a54489473284dafdb1303cfeefd8a38d4b1`
(branch `feature/iso-rust-frontier`), hafley-observe `e81d2812ad50457567ee41042d67b06c3b02d726`
(v0.1.2, default features off). The shared stress runner now resolves
hafley-observe from the same correlated checkout as production,
`/Users/chrishafley/projects/hafley-rs-wt/main-codex-attribution` at `0371e48f`;
all 23 lab tests passed after this dependency-path change.

---

## 1. Public type signatures

The public surface lives in `src/0_types.rs` and `src/lib.rs`:

```rust
pub type Cell = i64;
pub type Row = Vec<Cell>;
pub type ProgramId = u32;

#[derive(...)] pub struct RelId(pub u32);
#[derive(...)] pub struct RowId(pub u32);

#[derive(...)] pub enum Sign { Plus, Minus }
impl Sign { pub fn weight(self) -> i64 }              // +1 / -1

pub struct SourceChange { pub relation: RelId, pub sign: Sign, pub row: Row }
pub struct Frontier { pub id: String, pub changes: Vec<SourceChange> }
impl Frontier { pub fn empty(id: impl Into<String>) -> Self }

pub struct OutputChange { pub sign: Sign, pub row: Row }
pub struct OutputDelta { pub output: String, pub changes: Vec<OutputChange> }
// OutputDelta.changes is deduplicated and sorted by row cells ascending;
// every packet delta TSV matches this order field for field.

pub enum PlanNode {
    Scan { relation: RelId },
    Union { inputs: Vec<PlanNode> },
    Join { left: Box<PlanNode>, right: Box<PlanNode>, on_left: usize, on_right: usize },
    Aggregate { input: Box<PlanNode>, group_by: Vec<usize>, count: bool, sum: Option<usize> },
    Project { input: Box<PlanNode>, columns: Vec<usize> },
}
pub struct Program { /* name, output, root plan; Program::one(name, root) */ }

pub enum ErrorKind { UnknownRelation(RelId), UnknownProgram(ProgramId),
    ArityMismatch { expected: usize, actual: usize }, Unsupported { shape: &'static str } }
pub enum Stage { Validate, Apply, Install }
pub struct EngineError { pub stage: Stage, pub kind: ErrorKind,
    pub relation: Option<&'static str>, pub output: Option<&'static str> }  // Display names both

pub struct EngineStats { pub installed_programs: u32, pub operators: u64,
    pub arrangements: u64, pub source_rows: u64 }

pub trait FrontierEngine {
    type ProgramId;
    type Error;
    fn install(&mut self, program: &Program) -> Result<Self::ProgramId, Self::Error>;
    fn uninstall(&mut self, program: Self::ProgramId) -> Result<(), Self::Error>;
    fn apply(&mut self, program: Self::ProgramId, frontier: Frontier)
        -> Result<Vec<OutputDelta>, Self::Error>;
    fn snapshot(&self, program: Self::ProgramId, output: &str)
        -> Result<Vec<Row>, Self::Error>;
    fn stats(&self) -> EngineStats;
}
// Engine implements FrontierEngine with ProgramId = u32, Error = EngineError.
// Engine::snapshot_support(program, output) -> Result<Vec<(Row, i64)>, EngineError>
// is the inherent counterpart of the oracle `support` view (weights included).
```

### Associated-type justification

The packet fixes the domain: rows are integer cells (`Cell = i64`), rows have
integer identity (`RowId`), and join lookups are by typed cells. A generic
parameter renaming a fixed type is prohibited by the brief, so only two
associated types exist, each with two real call sites:

- `type ProgramId` — returned by `Engine::install` (`src/1_storage.rs`,
  install site) and consumed by every `apply`/`snapshot`/`uninstall` call site
  (`src/3_tx.rs` `Transaction::{apply, snapshot, snapshot_support}`, plus all
  five test files). It is `u32`, an arena index into `Engine::programs`.
- `type Error` — produced by `Engine::install` validation (including
  `Unsupported` shapes) and by `Engine::apply` (validation failures that abort
  a frontier). Both sites surface `EngineError`, whose `Display` prints the
  stage and, when known, the relation or output name — the observable
  analogue of the packet's "callback error" gate.

## 2. Installed-program and frontier lifetimes

- `Engine` owns all state: relation schemas, source rows, program arena,
  node arrangements, journal. Nothing borrows the caller. `Engine::new()` is
  the only constructor; `Engine: Default`.
- `ProgramId` is valid from `install` until `uninstall`. After uninstall the
  id is invalidated — `apply`/`snapshot` return
  `ErrorKind::UnknownProgram`, and the program's operators and arrangements
  are dropped from `stats()` while source rows survive.
- `Transaction<'e>` holds `&'e mut Engine`, so the borrow checker excludes
  concurrent scopes: a `Transaction`, a `Savepoint`, or a plain `apply` can
  exist one at a time. `Savepoint<'t, 'e>` nests inside its `Transaction`.
- A `Frontier` is owned by the callee for the duration of `apply` and is
  fully settled (consolidated, propagated, journaled) before the call
  returns; the returned `Vec<OutputDelta>` is a fresh allocation.
- Rollback is structural: `Scope`/`Savepoint` roll back on `Drop` if not
  settled (`tests/2_transactions.rs::unsettled_scope_rolls_back_on_drop`),
  and explicit `rollback_to`/`rollback` do the same deterministically.

## 3. Storage layout

`Engine` (`src/1_storage.rs`):

| field | contents |
|---|---|
| `relations: Vec<Relation>` | name, arity, row store, free list, value index |
| `programs: Vec<Option<ProgramState>>` | arena of installed programs |
| `journal: Vec<Undo>` | undo log for the innermost open scope |
| `marks: Vec<usize>` | journal length per open scope |
| `frontiers_applied: u64` | total frontiers (observability) |

- `Relation { name, arity, rows: Vec<Option<Row>>, free: Vec<u32>, by_value: HashMap<Row, Vec<u32>> }`
  — rows are slotted; deletion frees the slot and pushes it on the free list;
  reinsertion recycles LIFO (the undo log records the `recycled` flag so a
  rolled-back insert restores the exact free-list discipline).
  `by_value` maps a row value to every slot holding it, which is what makes
  duplicate rows distinct-but-equal (multiset semantics).
- Per node (`Node` enum, one variant per `PlanNode` shape):
  - `Scan`: no arrangement (0).
  - `Union`: one counts multiset `HashMap<Row, i64>` (1).
  - `Join`: counts multiset + two key indexes `KeyIndex` (3). `KeyIndex` is
    two-level: `HashMap<Row /*key*/, HashMap<Row /*whole row*/, i64>>`.
  - `Aggregate`: one `groups: HashMap<Row /*group key*/, Group{count, sum}>` (1).
  - `Project`: one counts multiset (1).
- These counts are the guardrail numbers surfaced by `stats()`: operators ==
  plan nodes, arrangements == materialized maps.

## 4. Exact read/write sequence per frontier

`apply_frontier` (`src/2_apply.rs`), under the calling scope's journal mark:

1. **Validate** — every `SourceChange` is checked against `Relation::arity`;
   removals are checked against current multiplicity (`by_value`). Any
   failure returns `EngineError { stage: Validate, .. }` before a single
   write; nothing is journaled, so the committed state is untouched.
2. **Consolidate** — changes net into `Net = BTreeMap<RelId, BTreeMap<Row, i64>>`
   (signed multiset). Net-zero entries vanish; over-removal was already
   rejected. This is where a same-frontier `+row, −row` batch collapses.
3. **Dependency filter** — a frontier touching none of a program's scanned
   relations short-circuits to an empty delta set (no spans, no work).
4. **Write source** — net inserts/removals mutate the relation; each
   mutation journals its exact inverse (`Undo::Insert { recycled }`,
   `Undo::Remove { row }`).
5. **Propagate bottom-up** with a memo (`memo: HashMap<NodeId, HashMap<Row, i64>>`);
   deltas are `HashMap<Row, i64>` signed multisets throughout.
   - Join: term1 = ΔL ⋈ R_post (the right key index is updated with ΔR
     *before* term1 runs); term2 = L_pre ⋈ ΔR with `pre = current − ΔL`, so
     the simultaneous-inputs cross-term lands exactly once
     (`tests/0_join_case.rs::simultaneous_join_inputs_produce_cross_term_once`).
   - Aggregate: per group key integrate Δcount/Δsum; transitions are birth
     (`+new`), death (`−old`), and both-live-and-changed (`−old +new`) —
     the row-level retract-old/add-new form the packet's `3d` TSV uses.
   - Project: maps each delta row through `columns` into the counts multiset.
   - Every node step emits a `maintain` span (node id, kind, delta rows).
6. **Emit** — per output, transitions become `OutputChange`s, sorted by row
   cells ascending, wrapped in `OutputDelta { output, changes }`; one
   `frontier` span records frontier id, input and output change counts.
7. **Commit** — the calling scope's mark is popped; the journal above the
   mark is discarded. A failure at any point unwinds to the mark (`unwind`
   replays `Undo` entries in reverse), leaving the previous committed
   snapshot readable.

Transactions (`src/3_tx.rs`): `Transaction::begin` marks; `apply` inside a
transaction auto-commits at a per-frontier sub-mark so an individual failing
frontier cannot poison the enclosing scope; `savepoint` nests a mark;
`rollback_to` unwinds to it; `commit` pops the outermost mark. `Drop`
without settlement rolls back.

## 5. What the oracle proves

Both standalone oracle commands are green (sqlite3 3.43.2), run from the lab
directory:

```
cd ../../plans/engine-iso && sqlite3 -batch -noheader -separator $'\t' :memory: \
  < 2_oracle.sql > /tmp/engine-iso-actual.tsv && diff -u 3_expected.tsv /tmp/engine-iso-actual.tsv
cd ../../plans/engine-iso && sqlite3 -batch -noheader -separator $'\t' :memory: \
  < 3b_aggregate.sql > /tmp/engine-iso-agg.tsv && diff -u 3c_aggregate_expected.tsv /tmp/engine-iso-agg.tsv
```

Both diffs: empty (oracle self-consistent before the engine was compared).

The engine is then compared against the same TSV pairs field-for-field:

- `tests/0_join_case.rs` (5 tests) walks all nine grant frontiers; each
  committed snapshot (with support weights) equals the matching
  `3_expected.tsv` group and each net signed delta equals the matching
  `3a_deltas.tsv` group, including the `EMPTY` frontiers. Frontier 6 applies
  inside a savepoint and rolls back; frontier 7 inside a transaction and
  rolls back; frontier 8 is the update pair. Additional tests pin support
  2→1 (no output) and 1→0 (exactly one retraction), the simultaneous-join
  cross-term appearing exactly once, update = retraction + addition, and a
  net-zero batch emitting nothing while leaving state and `source_rows`
  unchanged.
- `tests/1_aggregate_case.rs` (4 tests) walks all six aggregate frontiers
  against `3c`/`3d`, including the zero-group `4_empty` and the rolled-back
  `5_rollback`; duplicate source rows keep two distinct `RowId`s behind one
  value, deletion by value removes exactly one instance, and the group row
  change is the `−old +new` pair (row-sorted: `+[30,1,9]` before
  `-[30,2,18]`).
- `tests/2_transactions.rs` (6 tests): a frontier that fails mid-apply
  (valid first change, arity-invalid second) names stage `Validate` and
  relation `membership`, unwinds, and leaves the committed snapshot readable
  inside the still-open transaction and after commit; removal past
  multiplicity fails before any write; savepoint rollback discards only the
  savepoint's work; an unsettled scope rolls back on drop; uninstall drops
  arrangements and invalidates the program while source rows survive.
- `tests/3_guardrails.rs` (5 tests): grant program installs as 6 operators /
  5 arrangements, aggregate as 2 / 1, both in ONE engine through the same
  API with isolated snapshots; `PlanNode::Difference` and the two degenerate
  aggregates produce `Unsupported { shape }` at install with no fallback
  install; unknown programs and join-column overflow are rejected.
- `tests/4_observations.rs` (3 tests): through `hafley-observe`
  (`CountRecorder` + `with_default`), one `frontier` span per apply
  (including empty frontiers), union+join+project `maintain` spans per
  nonempty frontier, install events carrying the guardrail counts
  (operators=6, arrangements=5, outputs=1), and 11-vs-101 repeated frontiers
  showing constant per-frontier work and `Growth::Linear` total.
- `examples/0_frontiers.rs` runs both cases end-to-end plus 1000 repeated
  frontiers: **9.45 ms total, ~9.5 µs/frontier, peak RSS 2 MiB**
  (`hafley_observe::rusage::sample()`).

Totals: `cargo fmt --check` clean; `cargo test --offline` — **23 passed,
0 failed, 0 warnings**.

## 6. Guardrails vs measurements

- Guardrails (contractual, asserted): `stats().operators == plan nodes`
  (6 grant / 2 aggregate), `stats().arrangements == materialized maps`
  (5 / 1), `source_rows` multiset size, per-output `OutputDelta` sorted
  row-sorted and deduplicated, `Unsupported` shapes error rather than fall
  back, errors name stage + relation/output.
- Measurements (observed, not contractual): per-frontier wall time
  (~9.5 µs at 1000 frontiers), peak RSS (2 MiB), span counts (frontier ==
  applies; maintain == 3 per nonempty frontier), `Growth::Linear` for
  repeated frontiers.

## 7. Packet-review notes (open questions for the coordinator)

1. **"Callback error" gate** — the packet's gate is phrased for the
   SQLite-routes lab. Here it maps to an in-process apply error (validation
   failure mid-frontier): the unwinding leaves the prior snapshot readable,
   which `error_mid_frontier_leaves_committed_state_readable` proves.
2. **`3d` aggregate deltas are row-level** — retract-old/add-new pairs, not
   count/sum column deltas. The engine emits exactly that shape; confirmed
   against the TSV (e.g. `1_move_and_add` is `-[10,2,12] +[10,4,26]
   -[20,1,11]`).
3. **Duplicate source rows = multiset** — no primary-key enforcement; two
   rows with equal values are two derivations with distinct `RowId`s.
4. **SQL bytes / PROFILE / prepare-count gates are N/A in-process** — there
   is no SQL layer. Substituted observables: operator/arrangement counts,
   span entries, RSS, per-frontier timings, growth class.

## 8. Decisions worth remembering

- **Correlated path dependency** for `hafley-observe` (`default-features =
  false`): the stress runner links this lab with production and SQLite ISO in
  one Cargo package. All three must resolve the same path and package ID, so
  this lab uses `/Users/chrishafley/projects/hafley-rs-wt/main-codex-attribution/crates/hafley-observe`.
- **`PlanNode::Project` added**: the grant oracle arm projects the 4-column
  join to (person, resource) before the union; without projection the union
  would combine 2-column and 4-column rows (arity error). Projection maps
  deltas through `columns` and keeps one counts multiset.
- **`snapshot_support`** (inherent, not on the trait): the expected TSVs
  carry support weights, so the diffable snapshot is the support view; the
  trait's plain `snapshot` is the visible-rows view and is what
  `3_expected`'s row set matches ignoring weights.
- **Output ordering contract**: changes sorted by row cells ascending. The
  aggregate duplicate test initially asserted sign-first order
  (`-old` before `+new`) and failed; the engine contract (and both delta
  TSVs) are row-sorted, so the test was corrected, not the engine.
- **Undo journal, not state snapshots**: rollback cost is proportional to
  the frontier's writes, and `Undo::Insert { recycled }` preserves free-list
  LIFO discipline exactly across rollbacks.
