# IVM IR and two-engine design, draft 1

Inputs: `0_dbsp.md`, `1_substrait.md`, `2_turso.md`, `3_dd_feldera_materialize.md` in this
directory, and the existing code in `crates/frontier-engine` and `labs/20260923.3.dd-inside-sqlite`.
Order follows the planning rule: type signatures, instance lifetimes, storage and read/write
sequence, then the lab queue and open decisions.

## 0. Corrections to claims made in chat before the research

| claim made | research result | source |
|---|---|---|
| DD's type is `Collection<G, D, R>` | Up to 0.17 only. In 0.25.1: `Collection<'scope, T, C>`, alias `VecCollection<'scope, T, D, R> = Collection<'scope, T, Vec<(D,T,R)>>`. `Scope<'scope, T>` is a struct. | `3_…md` §1.1 |
| DDlog emitted typed Rust DD code per program | It emitted a `Program { nodes: Vec<ProgNode> }` value interpreted by a generic runtime. All collections carry type-erased `DDValue`. Per-program compile cost came from the generated `types` crates and rule fns. | `3_…md` §4.1 |

The DDlog runtime is the prior art for "static kernel compiled once, program as data".

## 1. Candidate table

| need | candidate | covers | does not cover | source |
|---|---|---|---|---|
| reference engine, in process | differential-dataflow 0.25.1 (MIT) | join, semijoin, antijoin, reduce, count, distinct, threshold, iterate/Variable, shared arrangements (`TraceAgent`) | window, topk as operators (built from reduce) | `3_…md` §1.3 |
| reference engine, in process | `dbsp` crate (MIT OR Apache-2.0, enterprise feature excluded) | same set plus explicit `delay`/`integrate`/`differentiate`, `window`, `topk`, `lag`, `rank`, `row_number` | lattice time; root clock is `()` | `0_…md` §2 |
| IR base | Substrait v0.103.1 (Apache-2.0), `substrait` crate 0.65.0 | 24 rel variants incl. anti/semi joins, aggregate, window, set ops; YAML function extensions; `Extension*Rel` with `Any` detail | recursion, delay, signed weights: absent from spec | `1_…md` |
| IR shape reference | Materialize `MirRelationExpr` (BSL 1.1, study only) | 15 variants shaped after DD: `Let`, `LetRec` with `limits`, n-ary `Join` with `equivalences`, `TopK`, `Threshold`, `ArrangeBy` | licence blocks reuse of code | `3_…md` §3.3 |
| incrementalization passes | Feldera SQL compiler (MIT, Java) | `IncrementalizeVisitor`, `OptimizeIncrementalVisitor` chain-rule rewrites, `RecursiveComponents` SCC grouping, reject list inside recursion | Datalog front end | `3_…md` §2.4-2.5 |
| SQLite-hosted state layout | Turso `core/incremental/` (MIT) | packed state table `(operator_id, zset_id, element_id, value, weight)`, update as delete+insert, uncommitted-overlay cursor, SELECT bootstrap | stock-SQLite hooks: Turso captures in VDBE opcodes | `2_…md` §8-9 |

## 2. Type signatures

### 2.1 IR (data, engine-free)

Op names follow MIR where MIR already names the concept.

```rust
type RelId = u32; type NodeId = u32; type ColId = u16;

enum Ty { Int, Real, Text, Blob, Id }                  // Id = interned surrogate
struct Schema { cols: Vec<Ty> }
struct Rel { id: RelId, name: String, schema: Schema, kind: RelKind }
enum RelKind { Source, Derived }

enum Expr { Col(ColId), Lit(Cell), Call(FuncRef, Vec<Expr>) }   // FuncRef = extension URN + name
enum Agg  { Count, Sum(ColId), Min(ColId), Max(ColId) }
enum WinFn { RowNumber, Rank, DenseRank, Lag(u32), Lead(u32), Sum(ColId), Count }
struct Order { col: ColId, desc: bool }

enum Op {
    Get(RelId),                                        // source or already-defined relation
    Mfp       { input: NodeId, filter: Vec<Expr>, map: Vec<Expr>, project: Vec<ColId> },
    Union     (Vec<NodeId>),
    Negate    (NodeId),
    Join      { inputs: Vec<NodeId>, equivalences: Vec<Vec<(u8, ColId)>> },
    Antijoin  { l: NodeId, r: NodeId, lk: Vec<ColId>, rk: Vec<ColId> },
    Reduce    { input: NodeId, key: Vec<ColId>, aggs: Vec<Agg> },
    Threshold (NodeId),                                // distinct = Threshold over a set
    TopK      { input: NodeId, key: Vec<ColId>, order: Vec<Order>, limit: u32 },  // argmax = limit 1
    Window    { input: NodeId, partition: Vec<ColId>, order: Vec<Order>, func: WinFn },
    Delay     (NodeId),                                // z^-1, the DL6/7 `pre`; parked until time lab
}

struct LetRec { ids: Vec<RelId>, bodies: Vec<NodeId>, limit: Option<u32> }  // one SCC; limit = depth cap
enum Stratum { Let { id: RelId, body: NodeId }, LetRec(LetRec) }
struct Program { rels: Vec<Rel>, nodes: Vec<Op>, strata: Vec<Stratum>, outputs: Vec<RelId> }

fn check(p: &Program) -> Result<Vec<Schema>, TypeError>;   // one Schema per NodeId
```

`check` rules:

| rule | source of the rule |
|---|---|
| join equivalence classes share one `Ty` | MIR `equivalences` |
| `Union` inputs share a schema | set semantics |
| `Sum/Min/Max` over `Int`/`Real` | aggregate typing |
| inside `LetRec`: no `Window`, `TopK`, `Delay` | Feldera `ValidateRecursiveOperators.java` |
| inside `LetRec`: `Negate`, `Antijoin`, `Reduce`, `Threshold` only over relations outside the SCC | stratified Datalog |
| `Rel.schema` equals body schema | declared type matches rule |

### 2.2 Row and weight

```rust
type Row = Box<[u32]>;     // interned cells; arity checked by `check`, not by rustc
type W = i64;              // Z-set weight, DD `R`, dbsp `ZWeight`
```

DDlog precedent: one erased value type, generic operators monomorphized once (`3_…md` §4.1).
Arity-specialized `[u32; N]` is an optimization lab, measured against `Box<[u32]>`.

### 2.3 Engine algebra

DD collections carry `'scope` and cannot leave `worker.dataflow` (`3_…md` §1.4). The algebra
impl therefore holds the lifetime; `lower` runs inside the engine's build callback.

```rust
trait Rel {
    type C: Clone;
    fn get(&mut self, r: RelId) -> Self::C;
    fn mfp(&mut self, c: Self::C, filter: &[Expr], map: &[Expr], project: &[ColId]) -> Self::C;
    fn union(&mut self, cs: &[Self::C]) -> Self::C;
    fn negate(&mut self, c: Self::C) -> Self::C;
    fn join(&mut self, cs: &[Self::C], eq: &[Vec<(u8, ColId)>]) -> Self::C;
    fn antijoin(&mut self, l: Self::C, r: Self::C, lk: &[ColId], rk: &[ColId]) -> Self::C;
    fn reduce(&mut self, c: Self::C, key: &[ColId], aggs: &[Agg]) -> Self::C;
    fn threshold(&mut self, c: Self::C) -> Self::C;
    fn topk(&mut self, c: Self::C, key: &[ColId], order: &[Order], limit: u32) -> Self::C;
    fn window(&mut self, c: Self::C, part: &[ColId], order: &[Order], f: &WinFn) -> Self::C;
    fn letrec(&mut self, s: &LetRec, p: &Program) -> Vec<Self::C>;   // engine-owned fixpoint
    fn output(&mut self, r: RelId, c: Self::C);
}

fn lower<A: Rel>(p: &Program, a: &mut A);             // written once

struct DdRel<'scope> { /* Scope<'scope, u64>, env: HashMap<RelId, VecCollection<'scope, u64, Row, W>> */ }
impl<'scope> Rel for DdRel<'scope> { type C = VecCollection<'scope, u64, Row, W>; /* … */ }

struct SqlRel<'c> { /* &'c Connection, generated DDL + settle SQL buffers */ }
impl<'c> Rel for SqlRel<'c> { type C = TableRef; /* … */ }
```

`letrec` is engine-owned. DD: `Variable::new` per id inside `iterate`'s `for<'inner>` scope, with
`lower` re-entered on a `DdRel<'inner>`. SQLite: round loop over the SCC's settle SQL until the
delta is empty or `limit` is hit. DDlog `ProgNode::Scc` is the precedent.

### 2.4 Lifecycle and host

```rust
struct Frontier { tick: u64, changes: Vec<(RelId, Row, W)> }
struct Delta    { tick: u64, changes: Vec<(RelId, Row, W)> }

trait Engine: Sized {
    fn install(p: &Program, host: &mut impl Host) -> Result<Self, EngineError>;
    fn settle(&mut self, f: Frontier) -> Result<Delta, EngineError>;
    fn snapshot(&self, r: RelId) -> Result<Vec<(Row, W)>, EngineError>;
}

trait Host {
    fn next(&mut self) -> Option<Frontier>;            // Raw: caller. Plugin: sqlite_ext collector at xSync
    fn sink(&mut self, d: &Delta) -> Result<(), EngineError>;   // Raw: returned. Plugin: result tables, same txn
}

struct Runtime<E: Engine, H: Host> { engine: E, host: H }
```

| | `Raw` | `Plugin` |
|---|---|---|
| `Dd` | compiler facts at comptime | `frontier-dd-ext` shape |
| `Sqlite` | `frontier-engine::Program` shape | `frontier-ext` shape |

## 3. Instance lifetimes

| instance | begins | ends | holds |
|---|---|---|---|
| `Program` | compiler emits, `check` passes | never mutated; lives with the `.db` | IR rows |
| `DdRel<'scope>` | `worker.dataflow` closure entry | closure return | collections |
| `Dd` engine | `install` | process exit | `InputSession`, `ProbeHandle`, `TraceAgent`s (scope-free) |
| `SqlRel<'c>` | `install` | `install` return | SQL text being generated |
| `Sqlite` engine | `install` or reattach at connection open | connection close | prepared statements, catalog row |
| `Plugin` host | extension load | connection close | collector registration |
| `Frontier` | COMMIT, or caller | `settle` return | signed source rows |
| interned `Id`s | first sight of a value | never reused | dictionary rows |

## 4. Storage and read/write sequence

### 4.1 Tables in the sealed `.db`

| table | key | written by | read by |
|---|---|---|---|
| `ivm_rel`, `ivm_col`, `ivm_node`, `ivm_expr`, `ivm_stratum` | ids | compiler, once at seal | runtime at install/reattach |
| `ivm_value(id INTEGER PRIMARY KEY, v)` + unique index on `v` | `id` | interner, append only | engines, readers of results |
| source tables | per relation | application | collector, bootstrap |
| result tables `(cols…, __w)` | cols | engine `sink` | application |
| `ivm_state(op, zset, element, value, w)` | `(op, zset, element)` | SQLite engine only | SQLite engine only |
| `ivm_catalog(frontier, install)` | singleton | engine | engine, reattach |

`ivm_state` follows Turso's packed layout (`2_…md` §3). The DD engine keeps equivalent state in
`TraceAgent`s in memory and rebuilds them from source tables at install.

### 4.2 Sequence per COMMIT, Plugin host

1. Application writes source rows inside its transaction.
2. `sqlite_ext` collector buffers row images per table.
3. xSync: collector builds one `Frontier`, calls `Engine::settle`.
4. Engine reads `ivm_state` (SQLite) or its traces (DD), computes `Delta`.
5. `Host::sink` writes `Delta` to result tables and bumps `ivm_catalog.frontier`, same transaction.
6. Pager commits. On rollback SQLite discards 4-5 for the SQLite engine; the DD engine detects the
   `frontier` mismatch on the next batch and rebuilds (existing behaviour, `labs/20260923.3…/HYPOTHESIS.md`).

### 4.3 Seal sequence, comptime

1. Compiler builds `Program`, runs `check`.
2. `Runtime<Dd, Raw>` installs, settles the compiler's facts as frontier 0.
3. Writes `ivm_*` program rows, `ivm_value`, source tables, result tables into a fresh `.db`.
4. Runtime opens the `.db`, loads the plugin, reattaches, bootstraps `ivm_state` from source rows,
   and checks result tables equal its own recompute (`snapshot` vs stored rows).

Uniqueness conditions: one `ivm_value` row per distinct value; one result row per visible key with
`__w > 0`; one `ivm_catalog` row; `frontier` strictly increasing per committed batch.

## 5. Lab queue

| # | lab | gate |
|---|---|---|
| 1 | `lab-20260924-dd-vs-dbsp-access-case` | both engines pass `plans/engine-iso` oracle TSVs; record RSS, time per frontier, allocations |
| 2 | `lab-20260924-ir-check` | `Program` + `check` over the access case, the aggregate case, `2_partial.dl7` lowered by hand; every `check` rule has a failing fixture |
| 3 | `lab-…-lower-dd` | `lower::<DdRel>` replaces the hardcoded `'access'`/`'team_cost'` in `frontier-dd-packet`; same oracle |
| 4 | `lab-…-lower-sqlite` | `lower::<SqlRel>` replaces `plan.rs` SQL parsing as the install path; same oracle |
| 5 | `lab-…-seal-roundtrip` | section 4.3 end to end; stored results equal runtime recompute |
| 6 | `lab-…-substrait-emit` | `Program` ↔ Substrait with `Extension*Rel` for `LetRec`/`Delay`; round-trip equality |

## 6. Open decisions

1. `Op` naming: MIR-aligned names above (`Get`, `Mfp`, `Threshold`, `LetRec`) vs names from chat
   (`Scan`, `Map`/`Filter`, `Distinct`, `Stratum`).
2. Reference engine for lab 1 onward: DD 0.25.1, `dbsp`, or both.
3. Substrait: IR serialization from lab 6, or never.
4. `Delay` and time: parked until the clock-checker work.
