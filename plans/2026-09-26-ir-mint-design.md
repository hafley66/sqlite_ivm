# IR Mint design

Issue: `ir-mint-op` (shown read-only with `issuectl --root /Users/chrishafley/projects/sprefa show ir-mint-op`). The issue specifies nested terms in `i64` cells, product constructor tables, sum variant tables with a shared ID space, `Mint(functor, args) -> id`, insert-or-get behavior, and an open `term_lt` ordering choice.

Implementation target: the `feature/ivm-crate-promotion` versions of `ivm-ir`, `ivm-engine`, `ivm-dd`, and `ivm-sqlite`. Existing-code references below are file:line in that branch unless an absolute Sprefa path is shown.

## 1. Type signatures

Current IR cells are `i64` (`Cell`), rows are `Vec<Cell>`, and relation columns are `Ty::{Int, Id}` (`crates/ivm-ir/src/0_ir.rs:10-20`). The current `Op` has `Get`, `Mfp`, `Join`, and the other relational operators, but no minting operator (`crates/ivm-ir/src/0_ir.rs:83-131`).

Candidate operator signature, expressed in the existing `Op` style:

```rust
Mint {
    input: NodeId,
    functor: RelId, // or a stable FunctorId / name; open representation choice
    args: Vec<ColId>,
}
```

```text
// Read each input row; select its args columns.
// Lookup (functor, selected args) in the engine's interner.
// Insert a new constructor row and id if the key is absent.
// Emit the input row with the id appended, or emit only the id; output shape is open.
```

Open shape detail: `args: Vec<ColId>` gives `Mint` the same input-column vocabulary as joins and reductions; `Vec<Expr>` would permit literal and computed arguments in the operator. The output could preserve the input columns and append the ID, or project to the ID alone. Current `Mfp` appends map columns then applies its projection, while `Join` concatenates input columns (`crates/ivm-ir/src/0_ir.rs:86-103`).

The existing algebra is named `Rel`, with associated collection type `C`; each operation lowers a relational node into `C` (`crates/ivm-engine/src/1_rel.rs:53-75`). Candidate method:

```rust
fn mint(&mut self, c: Self::C, functor: RelId, args: &[ColId]) -> Result<Self::C, EngineError>;
```

```text
// For each weighted input row, intern the selected argument tuple.
// Preserve its weight and return the row shape defined for Op::Mint.
// Make the constructor tuple observable as an intern_snapshot relation.
```

Open method details: whether `functor` is a `RelId`, a dedicated `FunctorId`, or an interned name; whether the trait receives selected columns or already projected arguments; and how the output constructor row reaches `intern_snapshot`. `lower_node` dispatches each current `Op` to the matching `Rel` method and memoizes the result (`crates/ivm-engine/src/1_rel.rs:127-184`).

The current constructor/variant catalog has no dedicated metadata: `RelKind` is only `Source | Derived`, and `Relation` contains `id`, `name`, `cols`, and `kind` (`crates/ivm-ir/src/0_ir.rs:22-34`). Candidate additions to resolve:

| Choice | Candidate representation |
|---|---|
| Constructor table as a relation | Add `RelKind::Constructor { functor, variant_group }`; each relation stores `(id, field…)`. `variant_group` identifies sum variants whose IDs share one space. |
| Explicit program metadata | Add `Program.constructors: Vec<Constructor>` with `id`, `functor`, `variant_group`, and argument types; leave constructor tables out of `RelKind`. |
| Functor arity | Store arity in metadata or derive it from the constructor relation's column count. |

The current `Program` fields are `rels`, `nodes`, `strata`, and `outputs` (`crates/ivm-ir/src/0_ir.rs:146-152`). Whether the selected constructor metadata adds a field to `Program`, or is completely carried by constructor `Relation`s and `Mint` nodes, remains open. Any new enum or field changes the serialized IR shape; the current IR types derive Serde serialization (`crates/ivm-ir/src/0_ir.rs:16-34, 83-152`).

## 2. Instance timelines

### Differential Dataflow

| Time | Existing engine lifetime | Interner implication to settle |
|---|---|---|
| Install | `Dd::install` starts a worker thread; the worker clones the `Program`, creates the timely dataflow, source input sessions, source guards, lowers the program, and creates output traces (`crates/ivm-dd/src/2_dd.rs:345-384, 421-457`). | A worker-owned interner can be created during dataflow construction. Whether it is one map for all constructors or split per variant group is open. |
| Settle | The `Dd` handle sends `Settle`; the worker guards source changes, updates inputs, advances the epoch, steps until the probe passes, and compacts traces (`crates/ivm-dd/src/2_dd.rs:469-494`). | New IDs and constructor rows must be visible to all Mint nodes in this worker before results are read. Transaction rollback semantics for Mint state are open. |
| Reattach / drop | `Dd::drop` sends `Stop` and joins the worker (`crates/ivm-dd/src/2_dd.rs:406-412`). The public `Engine` trait has install/settle/snapshot only, with no reattach method (`crates/ivm-engine/src/1_rel.rs:46-51`). | A stopped worker loses in-memory maps and arrangements. Reattach behavior would require rebuilding from persistent constructor rows or replaying a stable ID mapping; the mechanism is open. |

### SQLite

| Time | Existing engine lifetime | Interner implication to settle |
|---|---|---|
| Install | `Sqlite::install` takes a host connection, creates source tables, calls `install_ir_unwatched`, and stores the returned `SqlProgram`, IR, output, threshold flag, and tick in the engine handle (`crates/ivm-sqlite/src/4_engine.rs:10-16, 38-75`). `catalog::install_ir` lowers IR and installs its plan (`crates/ivm-sqlite/src/catalog.rs:261-280`). | Constructor tables and unique indexes can be installed in the database at this point. Ownership/name scoping for shared constructor dictionaries is open. |
| Settle | SQLite opens savepoint `ivm_engine_frontier`, validates and writes source changes, calls `SqlProgram::settle`, then releases on success or rolls back on error (`crates/ivm-sqlite/src/4_engine.rs:78-82, 84-168, 199-211`). | Mint insert-or-get must participate in that same savepoint for its constructor writes to agree with a failed or successful frontier. |
| Reattach | The existing SQL catalog stores serialized program JSON and install/frontier metadata; plan tables are created with `IF NOT EXISTS` (`crates/ivm-sqlite/src/catalog.rs:318-335, 727-832`). The `ivm_engine::Engine` interface has no reattach method (`crates/ivm-engine/src/1_rel.rs:46-51`); `Sqlite` currently reconstructs a fresh handle only through `install` (`crates/ivm-sqlite/src/4_engine.rs:38-75`). | Database-resident constructor rows and indexes can survive handle loss. Whether they are catalog-owned, program-owned, or global, and how a future reattach API validates/reuses them, is open. |

The proposed `intern_snapshot` exposes constructor rows as relations. The issue names that view but does not define whether it is a normal `Get` relation, a dedicated snapshot API, or part of engine snapshots; this remains open.

## 3. Storage, sequence of reads and writes, and uniqueness

### Layout candidates

| Engine | Constructor storage | Lookup key |
|---|---|---|
| Differential Dataflow | One interner keyed by `(functor, args)` plus constructor relation rows `(id, field…)`; sum variants draw IDs from their shared group. The existing worker owns source guards and output traces, built in one dataflow (`crates/ivm-dd/src/2_dd.rs:415-457`). | Hash/equality lookup on `(functor, args)`; exact key container and the relation/arrangement boundary are open. |
| SQLite | A table per product constructor, or a shared table with a functor/variant discriminator; sum variant tables reference a common ID space. Existing plans create persistent program tables and unique indexes (`crates/ivm-sqlite/src/catalog.rs:736-832`). | Unique constraint over `(functor, args…)`, or over `args…` for a per-functor table. Exact schema is open. |

The table layout in the issue is `ctor(id, field…)` for products, and separate variant tables sharing the ID space for sums. The unique identity requested by the issue is `(functor, args)`, independent of how the physical tables encode `functor`.

### One settle, one Mint row

1. Read the weighted input row and select its constructor arguments.
2. Probe the engine's interner for `(functor, args)`.
3. On a hit, read the existing ID and emit the input row with that ID.
4. On a miss, allocate an ID, write `(id, args…)` to the constructor/variant relation, record `(functor, args) -> id`, and emit that ID.
5. Consolidate resulting relational updates with the enclosing settle; for SQLite the Mint writes share the existing frontier savepoint (`crates/ivm-sqlite/src/4_engine.rs:78-82, 199-211`).

SQLite issue sketch: `INSERT … ON CONFLICT DO NOTHING` using a unique index on `(functor, args…)`, followed by `SELECT id WHERE functor=? AND args…=?`. In a per-functor table the functor predicate is implicit. ID allocation, handling a simultaneous first insert, and whether same-frontier duplicates emit one or repeated weighted rows are implementation details to specify.

### Retractions and ID lifetime

The key questions are open:

- **Monotone dictionary:** once `(functor, args)` is assigned an ID, retain the mapping and constructor row even when all user rows referencing it retract. A later Mint of the same tuple returns the same ID. Retraction affects user relations, not interned terms.
- **Reference-counted constructor rows:** retract the constructor row when its live support reaches zero. The ID allocator still needs a no-reuse or generation rule so a stale reference cannot name a different term. A later Mint may recover the prior mapping or allocate another ID.
- **Program lifetime versus database lifetime:** a DD worker's mapping ends when the worker stops unless checkpointed; SQLite rows can persist in the database. Whether IDs persist across reinstall/reattach and across programs remains open.

These choices also decide whether `intern_snapshot` reports all terms ever minted or only terms with live constructor support.

## 4. `TermLt`

Sprefa's `Universe` uses `IndexSet<Term>` and `TermId(u32)`; `intern` returns the term's insertion index (`/Users/chrishafley/projects/sprefa-wt/sqlite-perf-main/src/_6_eval/_0_term.rs:10-14, 28-33, 66-69`). Its structural `cmp` walks compound arguments recursively and applies the documented standard order (`/Users/chrishafley/projects/sprefa-wt/sqlite-perf-main/src/_6_eval/_0_term.rs:172-220`). `Order::TermLt` is the order selected by folds (`/Users/chrishafley/projects/sprefa-wt/sqlite-perf-main/src/_6_eval/_1_program.rs:41-54, 58-63`); `any_lt_row` compares through `Universe::cmp`, not numeric ID order (`/Users/chrishafley/projects/sprefa-wt/sqlite-perf-main/src/_6_eval/_4_kernel.rs:255-260`).

| Option | Comparison | Insert between existing terms |
|---|---|---|
| Dictionary-backed ordering | Resolve IDs to constructor/atom dictionary rows and recursively compare functor/variant and arguments. `[a,b] < [a,c]` because `b < c`. | Assign the new term's ordinary unique ID and retain existing IDs. Comparison needs dictionary reads/traversal; an ordered SQL index needs a comparator or stored order key. Updating existing references is unnecessary. |
| IDs assigned in term order | Compare IDs directly, with ID order maintained to match structural term order. | A dense sequence needs room between neighbors; inserting a middle term can renumber later IDs and rewrite every relation/arrangement reference and affected index. Sparse/order-maintenance labels defer relabeling, with a label-exhaustion/rebalance case. |

Worked IDs:

| Allocation/order case | `[a,b]` | `[a,c]` | Result of `TermLt([a,b], [a,c])` |
|---|---:|---:|---|
| Dictionary comparator; IDs reflect insertion order `[a,c]`, then `[a,b]` | 42 | 41 | true, from recursive dictionary comparison |
| IDs assigned in term order | 41 | 42 | true, from integer comparison |

For a middle insertion `[a,b] < [a,b½] < [a,c]`, dictionary comparison adds a dictionary entry and comparison work; ordered IDs need an unused label in the gap or relabeling. Whether Min/Max and SQL ordering use an engine custom comparator, a persisted order key, or term-ordered IDs remains open.

## 5. Worked example: `len([a,b])`

The issue's constructor example assigns `nil(1)`, `cons(2,b,1)`, and `cons(3,a,2)` and matches by joining `cons.id` to `3` (`ir-mint-op`, Worked example). The numbered graph below makes the required IR flow explicit. `Mint` is shown with the candidate input/columns signature above; the choice of preserving input columns or projecting only the minted ID is still open. `Unit`, `A`, and `B` denote one-row relations carrying `()` / `a` / `b`.

Relations: `R0 Unit()`, `R1 A(value:Id)`, `R2 B(value:Id)`, `R3 Nil(id:Id)`, `R4 Cons(id:Id, head:Id, tail:Id)`, `R5 Len(list:Id, length:Int)`. `Nil` and `Cons` share the list term ID space.

```text
00 = Get(R0)
01 = Mint { input: 00, functor: Nil,  args: [] }                 // Nil(1)
02 = Get(R2)
03 = Join { inputs: [02, 01], equivalences: [] }                 // B × Nil
04 = Mfp  { input: 03, map: [], project: [0, 1] }                 // (b, nil_id)
05 = Mint { input: 04, functor: Cons, args: [0, 1] }               // cons(2, b, 1)
06 = Get(R1)
07 = Join { inputs: [06, 05], equivalences: [] }                 // A × (b, nil_id)
08 = Mfp  { input: 07, map: [], project: [0, 3] }                 // (a, id_of_[b])
09 = Mint { input: 08, functor: Cons, args: [0, 1] }               // cons(3, a, 2)
10 = Get(R4)
11 = Join { inputs: [09, 10], equivalences: [[(0, 2), (1, 0)]] } // lookup outer cons by id
12 = Get(R4)
13 = Join { inputs: [11, 12], equivalences: [[(0, 5), (1, 0)]] } // lookup tail cons by id
14 = Get(R3)
15 = Join { inputs: [13, 14], equivalences: [[(0, 8), (1, 0)]] } // verify final tail is nil
16 = Mfp  { input: 15, map: [Lit(2)], project: [2, 10] }          // Len(root_id, 2)
```

Node column positions in joins follow the existing rule that inputs concatenate and each equivalence lists `(input position, column)` (`crates/ivm-ir/src/0_ir.rs:98-103`). The example uses literal length `2` after matching two cons rows and the nil row; compiling general recursive `len` requires a recursive stratum and arithmetic, beyond this finite worked input.

## 6. Test cases for the lab random harness

The current generator tracks node arity/depth, creates shared DAG nodes, and generates `Mfp`, `Union`, `Join`, `Antijoin`, `Reduce`, and `TopK`; it does not generate `Mint` (`labs/20260924.0.the-gang-runs-a-program-as-data-through-differential-dataflow/tests/random/1_gen.rs:12-52`). The random suite compares DD to its oracle, runs metamorphic permute/split/value cases, and optionally runs SQLite against the oracle (`tests/2_random.rs:29-70`). Mint cases should extend generation's type/arity tracking and oracle evaluation.

| Case | Input | Expected | Why |
|---|---|---|---|
| Repeated mint | Two input rows request the same `(cons,[a,b])` | Both carry one identical ID; one logical constructor row | Checks insert-or-get and `(functor,args)` uniqueness. |
| Distinct functors | Mint `(left,[a])` and `(right,[a])` in one shared sum ID space | Distinct IDs; each variant row has its own functor/variant tag | Checks functor participation in uniqueness and shared ID allocation. |
| Nested list round trip | Mint `nil`, then `cons(b,nil)`, then `cons(a,cons(b,nil))`; join by IDs to deconstruct | Reconstructed fields are `a`, `b`, final `nil`; `len` is `2` | Exercises Mint rows as joinable relations. |
| Join multiplicity | Duplicate paths reach a Mint input row with net weight `+2` | One unique ID mapping; emitted relational weight follows the input's consolidated weight | Separates identity creation from differential multiplicity. |
| Retract all references | Insert a row referencing `cons(a,nil)`, then retract that source row | **Open:** monotone dictionary expects constructor row and mapping to remain; live-support storage expects them to retract. | Pins down the ID death/reuse policy and `intern_snapshot` contents. |
| Remint after retraction | Retract every reference, then mint the same `(cons,[a,nil])` again | **Open:** stable dictionary expects the prior ID; fresh allocation may return a new ID while preserving structural equality. | Tests retraction semantics across a settle boundary. |
| Term order independent of allocation | Mint `[a,c]` before `[a,b]`, then compare them with `TermLt` | Structural order says `[a,b] < [a,c]` even if `[a,b]` has the larger insertion ID. | Detects accidental use of insertion IDs as term order. |
| Frontier rollback | First Mint a new tuple, then force another operation in that frontier to fail | No partial Mint mapping/constructor row remains after failed settle | Checks transaction/worker rollback alignment. |
| Engine agreement | Generate bounded nested constructors, joins, and insert/retract frontiers | DD, SQLite, and an independent structural-term oracle agree after decoding IDs; raw numeric IDs may differ unless assignment order is part of the contract. | Extends the existing randomized differential and oracle path (`tests/2_random.rs:29-70`). |

## Open choices

- `Mint` inputs and output columns; `functor` identity and arity metadata.
- Constructor relation representation, variant ID-space metadata, and whether `Program` gains fields.
- Whether intern tables are global, per program, or per engine instance.
- Interner creation and restore path for DD worker restart and future SQLite reattach.
- Retraction policy, ID reuse, and `intern_snapshot` visibility.
- `TermLt` representation, SQLite ordering mechanism, and behavior under middle insertion.
