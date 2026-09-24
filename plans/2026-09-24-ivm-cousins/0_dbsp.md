# DBSP: paper definitions, `dbsp` crate surface, time model, IR mapping

Retrieved 2026-09-24.

| Source | Pin |
|---|---|
| Paper | arXiv 2203.16684 v1 (30 Mar 2022), https://arxiv.org/abs/2203.16684. Only v1 is listed on the arXiv abstract page. |
| Published venue | PVLDB 16(7), 2023, pp. 1601-1614. UNVERIFIED (page range and whether section numbers match arXiv v1). |
| Crate source | https://github.com/feldera/feldera/tree/be8df0c175f772f1e0ec5cc9f5454bd3dbf042bb/crates/dbsp (main @ `be8df0c1`, pushed 2026-09-24) |
| Crate release | crates.io `dbsp` max_version `0.354.0` (2026-09-22); workspace `Cargo.toml` on main says `0.356.0` |

All section / definition / proposition numbers below are from arXiv v1.

---

## 1. Paper: core definitions

### 1.1 Streams and primitive operators (Section 2)

| Term | Formula | Meaning | Cite |
|---|---|---|---|
| Stream | `S_A = { s : N -> A }`; `s[t]` is the value at time `t` | Infinite sequence indexed by natural-number logical time | Def 2.1, Section 2.1 |
| Stream operator | `T : S_A0 x ... x S_An-1 -> S_B` | Function from streams to a stream | Def 2.2 |
| Lifting `↑f` | `(↑f)(s)[t] = f(s[t])` | Apply scalar function pointwise per time step | Def 2.3 |
| Distributivity | `↑(f ∘ g) = (↑f) ∘ (↑g)` | Lifting commutes with composition | Prop 2.4 |
| Abelian group requirement | `(A, +, 0, -)` commutative group | Stream values must form an abelian group | Section 2.2 |
| Delay `z^-1` | `z^-1(s)[t] = 0_A if t = 0; s[t-1] if t >= 1` | Shift stream one step; emits zero first | Def 2.5 |
| Time-invariance | `S(z^-1(s)) = z^-1(S(s))` | Operator commutes with delay | Def 2.6 |
| zpp | `f(0_A) = 0_B` | Zero-preservation; `↑f` is time-invariant iff `zpp(f)` | Def 2.7 |
| Causal | `∀i<=t. s[i]=s'[i] ⇒ S(s)[t]=S(s')[t]` | Output at `t` depends on inputs at `<= t` | Def 2.8 |
| Strict | `∀i<t. s[i]=s'[i] ⇒ F(s)[t]=F(s')[t]` | Output at `t` depends on inputs at `< t`; `z^-1` is strict | Def 2.9 |
| Fixed point of strict op | `α = F(α)` has a unique solution `fix α.F(α)` | Feedback loops closed through strict ops are well defined | Prop 2.10, Lemma 2.11, Cor 2.12 |
| Linear | `S(a + b) = S(a) + S(b)` (group homomorphism) | Includes `S(0)=0`, `S(-a) = -S(a)` | Def 2.14 |
| LTI | linear + time-invariant | `↑f` of linear `f` is LTI; `z^-1` is LTI | Section 2.2 |
| Bilinear | `f(a+b,c)=f(a,c)+f(b,c)`, `f(a,c+d)=f(a,c)+f(a,d)` | Linear in each argument separately; example `↑(·)` multiplication, join | Def 2.15 |
| Feedback with LTI | `Q(s) = fix α.S(s + z^-1(α))` | Well defined and LTI when `S` is causal LTI | Prop 2.16 |
| Differentiation `D` | `D(s) = s - z^-1(s)` | Stream of changes | Def 2.17; causal, LTI (Prop 2.18) |
| Integration `I` | `I(s) = fix α.(s + z^-1(α))`; `I(s)[t] = Σ_{i<=t} s[i]` | Running sum; reconstitutes a stream from changes | Def 2.19, Prop 2.20; causal, LTI (Prop 2.21) |
| Inversion | `I(D(s)) = D(I(s)) = s` | `I` and `D` are mutual inverses | Thm 2.22 |

### 1.2 Incremental computation (Section 3)

| Term | Formula | Meaning | Cite |
|---|---|---|---|
| Incremental version | `Q^Δ = D ∘ Q ∘ I` | Operator that consumes input changes and emits output changes | Def 3.1 |
| Multi-input | `T^Δ(a, b) = D(T(I(a), I(b)))` | Integrate each input independently | Def 3.1 |
| Inversion | `Q ↦ Q^Δ` bijective; inverse `Q ↦ I ∘ Q ∘ D` | | Prop 3.2 |
| Invariance | `+^Δ = +`, `(z^-1)^Δ = z^-1`, `-^Δ = -`, `I^Δ = I`, `D^Δ = D` | Primitives are their own incremental version | Prop 3.2 |
| Push/pull | `Q ∘ I = I ∘ Q^Δ`; `D ∘ Q = Q^Δ ∘ D` | | Prop 3.2 |
| Chain rule | `(Q1 ∘ Q2)^Δ = Q1^Δ ∘ Q2^Δ` | Incrementalize each sub-query independently; generalizes to multi-input | Prop 3.2 |
| Add | `(Q1 + Q2)^Δ = Q1^Δ + Q2^Δ` | | Prop 3.2 |
| Cycle | `(λs. fix α. T(s, z^-1(α)))^Δ = λs. fix α. T^Δ(s, z^-1(α))` | Incremental feedback loop = feedback loop around incremental body | Prop 3.2 |
| Linear rule | `Q^Δ = Q` for LTI `Q` | Linear ops run directly on deltas, zero state | Thm 3.3 |
| Bilinear rule | `(a × b)^Δ = a × b + z^-1(I(a)) × b + a × z^-1(I(b))` | Rewritten on deltas: `Δ(a ⋈ b) = Δa ⋈ Δb + a ⋈ Δb + Δa ⋈ b` (with `a`, `b` the previously integrated state) | Thm 3.4 |

### 1.3 Z-sets and relational operators (Section 4)

| Term | Formula | Meaning | Cite |
|---|---|---|---|
| Z-set | `Z[A]`: functions `A -> Z` with finite support | Weighted multiset; group under pointwise `+`/`-` | Section 4.1 |
| isset | `m[x] = 1 ∀x ∈ m` | Z-set represents a set | Def 4.1 |
| ispositive | `m[x] >= 0 ∀x` | Z-set is a bag | Def 4.2 |
| distinct | `distinct(m)[x] = 1 if m[x] > 0; 0 otherwise` | Project to set; drops non-positive weights | Def 4.3 |
| Positive / monotone stream | `s[t] >= 0`; `s[t] >= s[t-1]` | `I(s)` monotone if `s` positive | Def 4.4 |
| Translation table | union = `distinct(a+b)`; difference = `distinct(a-b)`; projection `π`, filter `σ_P`, `map(f)` linear; `×`, `⋈` bilinear; intersection = equi-join on same schema | SQL set ops as Z-set circuits | Table 1, Section 4.2 |
| distinct pull-up | `Q(distinct(i)) = distinct(Q(distinct(i)))` for `σ, ⋈, ×`; `distinct(Q(distinct(i))) = distinct(Q(i))` for `σ, π, map, +, ⋈, ×` | Defer / consolidate `distinct` to end of chain | Prop 4.5, Prop 4.6 |
| Incremental distinct | `(↑distinct)^Δ(d) = ↑H(z^-1(I(d)), d)` | Only elements in the change `d` are inspected | Prop 4.7 |
| `H` | `H(i,d)[x] = -1 if i[x]>0 ∧ (i+d)[x]<=0; 1 if i[x]<=0 ∧ (i+d)[x]>0; 0 otherwise` | Detects sign crossing of multiplicity | Prop 4.7 |
| IVM algorithm | (1) translate via Table 1, (2) consolidate distinct, (3) lift (Prop 2.4), (4) wrap in `I`/`D`, (5) apply chain rule + Prop 3.2 | Mechanical incrementalization | Algorithm 4.8, Section 4.3; worked example Section 4.4, Fig 1 |
| Complexity | linear `T^Δ`: `O(C[t])` time, 0 space; `(↑distinct)^Δ`: `O(C[t])` time, `O(R[t])` space; bilinear: `O(C[t]^2)` time, `O(R[t])` space | `C[t]` = size of change, `R[t]` = size of integrated relation | Section 4.5 |

### 1.4 Recursion and nested streams (Sections 5, 6)

| Term | Formula | Meaning | Cite |
|---|---|---|---|
| Zero almost-everywhere | `∃t0. ∀t >= t0. s[t] = 0`; set `S̄_A` | Streams that terminate | Def 5.1 |
| `δ0` (stream introduction) | `δ0(v)[t] = v if t=0; 0_A otherwise` | Scalar to stream (impulse) | Section 5 |
| `∫` (stream elimination) | `∫(s) = Σ_{t>=0} s[t]` | Definite integral over `S̄_A`; implemented by iterating until first zero | Section 5; `∫ ∘ δ0 = id_A` |
| LTI | `δ0`, `∫` are LTI | | Prop 5.2 |
| Nested time domain | `i -> δ0 -> Q -> ∫ -> o` | Inner clock insulated by `δ0 ... ∫` | Prop 5.3 |
| Naive recursion circuit | `I -> δ0 -> (+ z^-1 feedback) -> ↑R -> ↑distinct -> D -> ∫ -> O` | Computes `O = fix x.R(I, x)` for stratified Datalog | Section 5.1, Thm 5.4 |
| Semi-naive | `I -> δ0 -> (↑R)^Δ -> (↑distinct)^Δ -> ∫`, loop via `z^-1` | Obtained by cycle rule; equals semi-naive evaluation | Eq (5.1), Section 5.1 |
| Nested stream | `S_{S_A} = N -> (N -> A)`; value `s[t0][t1]` | Matrix; rows = outer time, columns = inner time | Section 6 |
| Nested lifting | `(↑S)(s) = S ∘ s`; `↑↑f` doubly lifted scalar fn | | Section 6 |
| Timestamp order | `(i0,i1) <= (t0,t1) iff i0<=t0 ∧ i1<=t1` | Partial order used to define strictness over nested streams | Section 6 |
| `↑z^-1` strict | | Delays inner (column) dimension; `z^-1` delays outer (row) | Prop 6.1 |
| Lifting cycles | `↑(λs. fix α.T(s, z^-1(α))) = λs. fix α.(↑T)(s, (↑z^-1)(α))` | Whole feedback circuits can be lifted | Prop 6.2 |
| Incremental recursive query | `I -> ↑δ0 -> (↑(↑T)^Δ)^Δ -> ↑∫ -> O`, loop via `↑z^-1` | Incremental maintenance of arbitrary recursive query | Eq (6.1), Section 6; example transitive closure Section 6.1, Fig 2 |
| Commutation | `I ∘ (↑I) = (↑I) ∘ I`; `D ∘ (↑D) = (↑D) ∘ D`; `z^-1` commutes with `↑z^-1` | | Appendix A.1 |
| Nested cost | space `Σ_{t2} ‖Σ_{t1} s[t1][t2]‖` | Stored state proportional to loop-iteration count | Section 6.2 |

### 1.5 Extensions: bags, aggregation, grouping, antijoin, windows, while (Section 7)

| Term | Formula | Meaning | Cite |
|---|---|---|---|
| UNION ALL | Z-set `+` without `distinct` | Bags are Z-sets | Section 7.1 |
| Aggregation fn | `a : Z[A] -> R` | Aggregate over a Z-set | Section 7.2 |
| COUNT | `a_COUNT(s) = Σ_{x∈s} s[x]` | Linear | Section 7.2 |
| SUM | `a_SUM(s) = Σ_{x∈s} x × s[x]` | Linear | Section 7.2 |
| makeset | `makeset(x) = 1 · x` | Scalar to singleton Z-set; non-linear; `(↑makeset)^Δ = D ∘ ↑makeset ∘ I`, O(1) per step | Section 7.2 |
| AVG | `π_c -> (a_SUM, a_COUNT) -> makeset -> σ_/` | Composite of linear aggregates | Section 7.2 |
| MIN (non-incremental) | `(↑a_MIN)^Δ = D ∘ ↑a_MIN ∘ I` | Work proportional to `R(s)` per step | Section 7.2 |
| ORDER BY | non-linear list-producing aggregate | Stated as not efficiently incrementalizable; future work | Section 7.2 |
| Indexed Z-set | `K -> Z[A]` = `Z[A][K]` | Group-by result; abelian group | Section 7.3 |
| Grouping `G_p` | `G_p(a)[k] = Σ_{x∈a. p(x)=k} a[x] · x` | Partition by key fn; linear for any `p` | Section 7.3 |
| `Agg_a` | `Agg_a(g) = Σ_{k∈K} a(k, g[k])` | Per-group aggregate then sum; only changed groups recomputed when `a` non-linear | Section 7.4 |
| flatmap | `Z[A][K] -> Z[A x K]`; linear | Inverse of partitioning | Section 7.4 |
| Antijoin | `O(v,z) :- I1(v,z), not I2(v)` = `distinct(I1 - (I1 ⋈ I2))` | Join followed by difference | Section 7.5 |
| Stream-relation join | `T(s,t) = I(s) ↑⋈ t` | ksqlDB-style | Section 7.6 |
| Window `W` | `W(v,θ)[t] = { x ∈ v[t] . ts(x) >= θ[t] - 1hr }` | With monotone `θ`, `W` moves inside `I`: bounded memory | Section 7.6.1 |
| While | `i -> δ0 -> (+ z^-1) -> ↑Q -> D -> ∫ -> x` | Relational while loop; incrementalized to `Δi -> ↑δ0 -> (↑↑Q)^Δ -> ↑D -> ↑∫ -> Δx` | Section 7.7 |

### 1.6 Stated relation to Differential Dataflow (Section 9)

Quoted from Section 9: "All DBSP operators are based on DD operators. DD's computational model is more powerful than DBSP, since it allows past values in a stream to be 'updated'. In contrast, our model assumes that the inputs of a computation arrive in the time order while allowing for nested time domains via the modular lifting transformer."

---

## 2. `dbsp` crate (feldera/feldera, `crates/dbsp`)

### 2.1 Package and license

| Field | Value | Source |
|---|---|---|
| name | `dbsp` | `crates/dbsp/Cargo.toml` |
| description | "Continuous streaming analytics engine" | `crates/dbsp/Cargo.toml` |
| edition | `2024` (crate); workspace `2021` | crate / root `Cargo.toml` |
| license | `MIT OR Apache-2.0` (workspace `[workspace.package]`) | root `Cargo.toml` |
| repo LICENSE file | MIT for open-source edition; code gated behind `feldera-enterprise` feature or documented Enterprise-only is excluded from MIT | https://github.com/feldera/feldera/blob/main/LICENSE |
| GitHub SPDX detection | `NOASSERTION` | `gh api repos/feldera/feldera` |
| MSRV | `1.96.1` | root `Cargo.toml` |
| Paper-era repo | `vmware/database-stream-processor` (MIT) | paper Section 8 |

Base URL for paths below: `https://github.com/feldera/feldera/blob/be8df0c175f772f1e0ec5cc9f5454bd3dbf042bb/crates/dbsp/src/`

### 2.2 Re-exports (`lib.rs`)

```rust
pub use algebra::{DynZWeight, ZWeight};
pub use circuit::{ChildCircuit, Circuit, CircuitBase, CircuitHandle, Consensus, DBSPHandle,
    NestedCircuit, RootCircuit, Runtime, RuntimeError, SchedulerError, Stream, WeakRuntime};
pub use operator::{CmpFunc, OrdPartitionedIndexedZSet, OutputHandle,
    input::{IndexedZSetHandle, InputHandle, MapHandle, ZSetHandle}};
pub use trace::{DBData, DBWeight, cursor::Position};
pub use typed_batch::{Batch, BatchReader, ..., IndexedZSet, IndexedZSetReader,
    OrdIndexedWSet, OrdIndexedZSet, OrdWSet, OrdZSet, Trace, TypedBox, ZSet};
pub use crate::time::Timestamp;
```

### 2.3 Circuit and stream types

| Item | Signature | File |
|---|---|---|
| `Stream` | `pub struct Stream<C, D> { stream_id, local_node_id, origin_node_id, circuit: C, val: RefStreamValue<D> }` (at most one value per step, "since our circuits are synchronous") | `circuit/circuit_builder.rs:716` |
| `ChildCircuit` | `pub struct ChildCircuit<P, T> where T: Timestamp { inner: Rc<CircuitInner<P>>, time: Rc<RefCell<T>> }` | `circuit/circuit_builder.rs:3100` |
| `RootCircuit` | `pub type RootCircuit = ChildCircuit<(), ()>;` | `circuit/circuit_builder.rs:3123` |
| `NestedCircuit` | `pub type NestedCircuit = ChildCircuit<RootCircuit, <() as Timestamp>::Nested>;` | `circuit/circuit_builder.rs:3125` |
| `IterativeCircuit` | `pub type IterativeCircuit<P> = ChildCircuit<P, <<P as WithClock>::Time as Timestamp>::Nested>;` | `circuit/circuit_builder.rs:3128` |
| `NonIterativeCircuit` | `pub type NonIterativeCircuit<P> = ChildCircuit<P, <P as WithClock>::Time>;` | `circuit/circuit_builder.rs:3131` |
| `RootCircuit::build` | `pub fn build<F, T>(constructor: F) -> Result<(CircuitHandle, T), DbspError> where F: FnOnce(&mut RootCircuit) -> Result<T, AnyError>` | `circuit/circuit_builder.rs:3201` |
| `Circuit` trait | `pub trait Circuit: CircuitBase + Clone + WithClock { type Parent; ... }` | `circuit/circuit_builder.rs:2033` |
| `Circuit::iterate` | `fn iterate<F, C, T>(&self, constructor: F) -> Result<T, SchedulerError> where F: FnOnce(&mut IterativeCircuit<Self>) -> Result<(C, T), SchedulerError>, C: AsyncFn() -> Result<bool, SchedulerError> + 'static;` | `circuit/circuit_builder.rs:2718` |
| `Circuit::fixedpoint` | `fn fixedpoint<F, T>(&self, constructor: F) -> Result<T, SchedulerError> where F: FnOnce(&mut IterativeCircuit<Self>) -> Result<T, SchedulerError>;` Fixed point detected by asking each operator `Operator::fixedpoint(scope)`; `Z1` checks conservatively for input and output both zero | `circuit/circuit_builder.rs:2733-2770` |
| `Circuit::iterative_subcircuit` / `non_iterative_subcircuit` | child with nested clock / same clock | `circuit/circuit_builder.rs:2636, 2643` |
| `Circuit::add_source`, `add_unary_operator`, `add_binary_operator`, `add_feedback` | low-level operator wiring | `circuit/circuit_builder.rs:2209, 2340, 2364, 2545` |

### 2.4 Z-set types

| Item | Signature | File |
|---|---|---|
| `ZWeight` | `pub type ZWeight = i64;` | `algebra/zset.rs:39` |
| `TypedBatch` | `#[repr(transparent)] pub struct TypedBatch<K, V, R, B> { inner: B, phantom: PhantomData<fn(&K, &V, &R)> }` (typed wrapper over dynamically typed batch) | `typed_batch.rs` |
| `OrdZSet` | `pub type OrdZSet<K> = TypedBatch<K, (), ZWeight, DynOrdZSet<DynData>>;` | `typed_batch.rs` |
| `OrdIndexedZSet` | `pub type OrdIndexedZSet<K, V> = TypedBatch<K, V, ZWeight, DynOrdIndexedZSet<DynData, DynData>>;` | `typed_batch.rs` |
| `OrdWSet` | `pub type OrdWSet<K, R, DynR> = TypedBatch<K, (), R, DynOrdWSet<DynData, DynR>>;` | `typed_batch.rs` |
| `IndexedZSet` trait | `pub trait IndexedZSet: Batch<R = ZWeight, DynR = DynZWeight, Time = (), InnerBatch = Self::InnerIndexedZSet>` | `typed_batch.rs` |
| `ZSet` trait | `pub trait ZSet: IndexedZSet<Val = (), DynV = DynUnit> { fn weighted_count(&self) -> ZWeight; }` | `typed_batch.rs` |
| File-backed variants | `FileZSet`, `FileIndexedZSet`, `FallbackZSet`, ... | re-exported from `typed_batch.rs` |

### 2.5 Operators (methods on `Stream`)

| Paper op | Method | Signature (abridged to essentials) | File |
|---|---|---|---|
| `σ_P` | `filter` | `impl<C: Circuit, B: FilterMap> Stream<C, B> { pub fn filter<F>(&self, f: F) -> Self where F: Fn(B::ItemRef<'_>) -> bool + 'static }` | `operator/filter_map.rs:78` |
| `map(f)` | `map` | `pub fn map<F, K>(&self, f: F) -> Stream<C, OrdWSet<K, B::R, B::DynR>> where K: DBData, F: Fn(B::ItemRef<'_>) -> K + Clone + 'static` | `operator/filter_map.rs:88` |
| index / `G_p` | `map_index` | `pub fn map_index<F, K, V>(&self, f: F) -> Stream<C, OrdIndexedWSet<K, V, B::R, B::DynR>> where F: Fn(B::ItemRef<'_>) -> (K, V) + 'static` | `operator/filter_map.rs:100` |
| flatmap | `flat_map`, `flat_map_index` | `pub fn flat_map<F, I>(&self, f: F) -> Stream<C, OrdWSet<I::Item, B::R, B::DynR>> where F: FnMut(B::ItemRef<'_>) -> I, I: IntoIterator` | `operator/filter_map.rs:127, 140` |
| `+` | `plus`, `minus`, `sum` | `pub fn plus(&self, other: &Stream<C, D>) -> Stream<C, D>`; `pub fn sum<'a, I>(&'a self, streams: I) -> Stream<C, D> where I: IntoIterator<Item = &'a Self>` | `operator/plus.rs:56, 82`; `operator/sum.rs:28` |
| `-` | `neg` | `pub fn neg(&self) -> Stream<C, D>` | `operator/neg.rs:20` |
| `(⋈)^Δ` | `join` (incremental) | `impl<C, K1, V1> Stream<C, OrdIndexedZSet<K1, V1>> { pub fn join<F, V2, V>(&self, other: &Stream<C, OrdIndexedZSet<K1, V2>>, join: F) -> Stream<C, OrdZSet<V>> where F: Fn(&K1, &V1, &V2) -> V + Clone + 'static }` | `operator/join.rs:123` |
| `(⋈)^Δ` indexed out | `join_index`, `join_flatmap`, `join_generic` | `join_index<F, V2, K, V, It>(...) -> Stream<C, OrdIndexedZSet<K, V>> where F: Fn(&K1,&V1,&V2) -> It, It: IntoIterator<Item=(K,V)>` | `operator/join.rs:185, 151, 350` |
| `↑⋈` (per step, no state) | `stream_join` | `pub fn stream_join<F, I2, V>(&self, other: &Stream<C, I2>, join: F) -> Stream<C, OrdZSet<V>> where I2: IndexedZSet<Key = I1::Key>` | `operator/join.rs:261` |
| antijoin | `antijoin` (incremental), `stream_antijoin` | `pub fn antijoin<I2>(&self, other: &Stream<C, I2>) -> Stream<C, I1> where I2: IndexedZSet<Key = I1::Key, DynK = I1::DynK>` ("excluding keys that are present in `other`") | `operator/join.rs:374, 336` |
| outer join | `outer_join`, `outer_join_default` | `outer_join<I2, F, FL, FR, O>(&self, other, join_func: F, left_func: FL, right_func: FR) -> Stream<C, OrdZSet<O>>` | `operator/join.rs:396, 215` |
| semijoin | `semijoin_stream` | `pub fn semijoin_stream<Keys, Out>(&self, keys: &Stream<C, Keys>) -> Stream<C, Out>` | `operator/semijoin.rs:28` |
| `(↑distinct)^Δ` | `distinct` (incremental), `hash_distinct` | `impl<C: Circuit, Z: IndexedZSet> Stream<C, Z> { pub fn distinct(&self) -> Stream<C, Z> }` | `operator/distinct.rs:38, 52` |
| `↑distinct` | `stream_distinct` | `pub fn stream_distinct(&self) -> Stream<C, Z>`: weight `> 0` -> 1, else drop | `operator/distinct.rs:20` |
| positive filter | `positive` | `pub fn positive(&self) -> Stream<C, Z> where Z: ZSet` | `operator/distinct.rs:70` |
| `Agg_a` incremental | `aggregate` | `impl<C, K, V> Stream<C, OrdIndexedZSet<K, V>> { pub fn aggregate<A>(&self, aggregator: A) -> Stream<C, OrdIndexedZSet<K, A::Output>> where A: Aggregator<V, <C as WithClock>::Time, ZWeight> }`; provided aggregators `Min`, `Max`, `Fold` | `operator/aggregate.rs:37`; `operator/dynamic/aggregate/{min,max,fold,average}.rs` |
| linear aggregate | `aggregate_linear` | `pub fn aggregate_linear<F, A>(&self, f: F) -> Stream<C, OrdIndexedZSet<Z::Key, A>> where A: DBWeight + MulByRef<ZWeight, Output = A>, F: Fn(&Z::Val) -> A` | `operator/aggregate.rs:209` |
| per-step aggregate | `stream_aggregate`, `stream_aggregate_linear` | `stream_aggregate<A>(&self, a: A) -> Stream<C, OrdIndexedZSet<Z::Key, A::Output>> where A: Aggregator<Z::Val, (), ZWeight>` | `operator/aggregate.rs:76, 128` |
| COUNT | `weighted_count`, `distinct_count` (+ `stream_*`) | `pub fn weighted_count(&self) -> Stream<C, OrdIndexedZSet<Z::Key, ZWeight>>` | `operator/count.rs:24, 76` |
| `z^-1` | `delay`, `delay_with_initial_value`, `delay_nested`; struct `Z1<T>`; feedback builders `DelayedFeedback<C, D>` (`new`, `stream`, `connect`) | `pub fn delay(&self) -> Stream<C, D> where D: Checkpoint + Eq + SizeOf + NumEntries + Clone + HasZero + 'static` | `operator/z1.rs:151, 167, 184, 222, 38` |
| `I` | `integrate`, `integrate_nested`, `accumulate_integrate` | `impl<C: Circuit, D: ...> Stream<C, D> { pub fn integrate(&self) -> Stream<C, D> }` | `operator/integrate.rs:85, 158, 189` |
| `D` | `differentiate`, `differentiate_nested`, `differentiate_with_initial_value` | `impl<C: Circuit, D: Checkpoint + SizeOf + NumEntries + GroupValue> Stream<C, D> { pub fn differentiate(&self) -> Stream<C, D> }` | `operator/differentiate.rs:38, 44, 105` |
| `δ0` | `delta0` | `pub fn delta0<CC>(&self, subcircuit: &CC) -> Stream<CC, D> where CC: Circuit<Parent = C>` | `operator/delta0.rs:22` |
| recursion Eq (6.1) | `ChildCircuit::recursive` | `pub fn recursive<F, S>(&self, f: F) -> Result<S::Output, SchedulerError> where S: RecursiveStreams<IterativeCircuit<Self>>, F: FnOnce(&IterativeCircuit<Self>, S) -> Result<S, SchedulerError>`. Doc: computes fixed point of `y = distinct(f(i+Δi, y))`; `distinct` inserted on `f` output; `δ0` must be applied by `f` to each imported stream | `operator/recursive.rs:262` |
| recursion, runtime arity | `recursive_dynamic` | `pub fn recursive_dynamic<F, K, V, B>(&self, arity: usize, f: F) -> Result<Vec<Stream<Self, TypedBatch<K, V, ZWeight, B>>>, SchedulerError>` | `operator/recursive.rs:448` |
| `RecursiveStreams` | `pub trait RecursiveStreams<C>: Clone { type Inner; type Output; ... }`; impl for `Stream<C, TypedBatch<K, V, ZWeight, B>>` and tuples | `operator/recursive.rs:14, 38, 71` |
| window `W` | `window` | `impl<K, V> Stream<RootCircuit, OrdIndexedZSet<K, V>> { pub fn window(&self, inclusive: (bool, bool), bounds: &Stream<RootCircuit, (TypedBox<K, DynData>, TypedBox<K, DynData>)>) -> Stream<RootCircuit, OrdIndexedZSet<K, V>> }`. Output = changes to window contents; `start_time` must be monotone; the window time key is separate from DBSP logical time | `operator/time_series/window.rs:66` |
| other time-series | `partitioned_rolling_aggregate`, `waterline_monotonic`, radix-tree aggregates | | `operator/time_series/`, `operator/dynamic/time_series/` |
| other group ops | `topk`, `lag`, `rank`, `row_number` | | `operator/group/` |
| asof join, star join, range join | `asof_join`, `multijoin/star_join`, `join_range` | | `operator/asof_join.rs`, `operator/dynamic/multijoin/`, `operator/join_range.rs` |
| exchange / sharding | `shard`, `gather`, `exchange` | | `operator/communication/` |

Naming convention in the crate: `stream_X` is the lifted per-step operator `↑X`; unprefixed `X` is the incremental `(↑X)^Δ` (from doc comments on `distinct`, `join`, `aggregate`).

---

## 3. Time model: `dbsp` vs differential-dataflow

### 3.1 `dbsp` time definitions (`time.rs`, `time/product.rs`, `algebra/lattice.rs`)

| Item | Definition | File |
|---|---|---|
| Logical time | "array of integers whose length is equal to the nesting depth"; root clock 1-D, starts at 0, +1 per tick; nested circuit 2-D | `time.rs` module doc |
| `Timestamp` | `pub trait Timestamp: DBData + PartialOrder + Lattice { const NESTING_DEPTH: usize; type Nested: Timestamp; type TimedBatch<B>; fn minimum(); fn clock_start(); fn advance(&self, scope: Scope) -> Self; fn recede(..); fn checked_recede(..); fn epoch_start(..); fn epoch_end(..); }` | `time.rs:62` |
| `advance` semantics | `(2,3).advance(0) == (2,4)`; `(2,3).advance(1) == (3,0)` (tick at level resets deeper levels) | `time.rs` doc |
| Root time | `impl Timestamp for ()` with `type Nested = Product<u32, u32>`, `TimedBatch<B> = B` | `time.rs` |
| `u32` time | `type Nested = Product<u32, u32>` | `time.rs` |
| `Product` | `pub struct Product<TOuter, TInner> { pub outer: TOuter, pub inner: TInner }`; `NESTING_DEPTH = TOuter::NESTING_DEPTH + 1`; `type Nested = Product<Self, u32>` | `time/product.rs:33, 85` |
| `Lattice` | `pub trait Lattice: PartialOrder { fn join(..); fn meet(..); }` "All logical times in DBSP must implement the `Lattice` trait" | `algebra/lattice.rs:23` |
| Lossy times | stored times are compacted to "just enough timing information"; `()` = untimed. `integrate_trace` stores an untimed trace (sum since last `clock_start`) | `time.rs` module doc |
| `IndexedZSet` | bound `Time = ()` | `typed_batch.rs` |

### 3.2 Comparison table

| Aspect | `dbsp` | differential-dataflow |
|---|---|---|
| Update tuple | batch of `(key, val, weight)`; stream value per tick; batch `Time = ()` at root | `(data, time, diff)` with explicit `time` per update |
| Root clock | Synchronous; one batch per tick; ticks totally ordered; `CircuitHandle::step` (`circuit_builder.rs:7811`) / `DBSPHandle::step` (`dbsp_handle.rs:1754`) processes tick `t` fully before `t+1` | Timely frontier-driven; timestamps from any `Lattice`; multiple timestamps may be in flight; operators act as frontiers advance |
| Order of inputs | Paper Section 9: "inputs of a computation arrive in the time order" | Updates may arrive at times not yet closed; past (not-yet-completed) times remain updatable until the frontier passes |
| Partial order | Only from nesting: `Product<outer, inner>` in nested circuits | Arbitrary lattice (e.g. `Product<T, u32>` in `iterate`, user-defined multi-dimensional times such as bitemporal) |
| Arrangement / trace state | Root traces untimed (integrated state); nested-circuit traces carry `Product` times (lossy) | Arrangements keep per-update times; compaction via `advance_by` / logical frontier to a lattice representative |
| Recursion | `recursive` / `fixedpoint` child circuit: inner clock runs to fixpoint per outer tick, then `clock_end` resets | `iterate` / `Variable` enter a scope with `Product<T, Iter>` timestamps; iterations for different outer times may overlap |
| Formal basis | Streams over abelian groups; `D`, `I`, `z^-1`, `↑` (Sections 2-3) | Lattice-timestamped collections (Abadi, McSherry, Plotkin "Foundations of differential dataflow", paper ref [2]) |
| Weight | `ZWeight = i64` for Z-sets; `OrdWSet<K, R, ..>` generic weight | `R: Semigroup` / `Abelian` (crate-generic diff type). UNVERIFIED for current trait names in DD master |

### 3.3 Concrete example: transitive closure with two input transactions

Input edges: tx0 = `{(a,b)}`, tx1 = `{(b,c)}`. Query: `R(x,y) :- E(x,y). R(x,z) :- E(x,y), R(y,z).`

`dbsp` (`RootCircuit` + `recursive`):

| Outer tick (root time) | Inner ticks (`Product` inner) | Inner-circuit deltas | Root output delta |
|---|---|---|---|
| 0 | 0 | `+(a,b)` | |
| 0 | 1 | `∅` -> fixed point, `clock_end` | `ΔR[0] = {(a,b)+1}` |
| 1 | 0 | `+(b,c)` | |
| 1 | 1 | `+(a,c)` (uses integrated state from tick 0 via `z^-1(I(..))` traces) | |
| 1 | 2 | `∅` -> fixed point | `ΔR[1] = {(b,c)+1, (a,c)+1}` |

Tick 1 starts only after tick 0 reaches its fixed point and returns control (`fixedpoint` doc, `circuit_builder.rs:2733`). Root-level output batches carry no time field.

differential-dataflow (`iterate` scope, timestamps `Product<u64, u64>` = (outer, iter)): updates emitted as

| Update | Time |
|---|---|
| `((a,b), +1)` | `(0, 0)` |
| `((b,c), +1)` | `(1, 0)` |
| `((a,c), +1)` | `(1, 1)` |

If both inputs are introduced before the input frontier advances past 0, the operator can hold updates at `(0,·)` and `(1,·)` concurrently; arrangements store the times and join at `lub((0,1),(1,0)) = (1,1)`. An update at time `(0,5)` and one at `(1,2)` are incomparable in the product order. The exact DD iteration counter type and API names (`iterate`, `Variable`, `enter`, `leave`) are from DD documentation knowledge, UNVERIFIED against current DD master in this session.

---

## 4. Mapping onto an operator IR (Scan / Map / Filter / Concat / Negate / Join / Antijoin / Reduce / Window / Distinct / Fixpoint / Delay)

| IR op | Paper construct | Linearity (paper) | Incremental rule | State held by incremental form | `dbsp` method(s) |
|---|---|---|---|---|---|
| Scan | input stream `Δt`; source of `I` | n/a | input already a delta stream | none (source); `I` when a downstream bilinear/distinct needs history | `RootCircuit::add_input_zset<K>(&self) -> (Stream<RootCircuit, OrdZSet<K>>, ZSetHandle<K>)` / `add_input_indexed_zset<K, V>` (`operator/input.rs:320, 349`); `ZSetHandle`, `IndexedZSetHandle` |
| Map | `↑map(f)`, `↑π` | linear (Table 1) | `Q^Δ = Q` (Thm 3.3) | none | `map`, `map_index`, `flat_map`, `flat_map_index` |
| Filter | `↑σ_P` | linear | `Q^Δ = Q` | none | `filter` |
| Concat | Z-set `+` (UNION ALL) | linear | `+^Δ = +` (Prop 3.2) | none | `plus`, `sum` |
| Negate | Z-set `-` | linear | `-^Δ = -` | none | `neg`, `minus` |
| Join | `⋈` equi-join | bilinear | `Δa⋈Δb + z^-1(I(a))⋈Δb + Δa⋈z^-1(I(b))` (Thm 3.4) | `I(a)`, `I(b)` (two traces) | `join`, `join_index`, `join_flatmap`, `join_generic`; per-step `stream_join` |
| Antijoin | `distinct(I1 - (I1 ⋈ I2))` | composite (join + minus + distinct) | chain rule over the composite | traces of join inputs + distinct state | `antijoin` (incremental), `stream_antijoin` |
| Reduce | `G_p` then `Agg_a` (Sections 7.3, 7.4) | `G_p` linear; `Agg_a` linear iff `a` linear in 2nd arg (COUNT, SUM); MIN/MAX via `D ∘ ↑a ∘ I` | linear: `Q^Δ = Q`; non-linear: recompute only changed groups | linear: none (paper), crate `aggregate_linear` stores per-key output UNVERIFIED; non-linear: integrated input per group | `aggregate`, `aggregate_linear`, `aggregate_linear_postprocess`, `weighted_count`, `distinct_count`; per-step `stream_aggregate*` |
| Window | `W(v, θ)` inside `I` with monotone `θ` (Section 7.6.1) | not linear in `θ` | moves inside `I` under monotone `θ` | bounded by window range | `window(inclusive, bounds)`, `partitioned_rolling_aggregate*`, `waterline_monotonic` |
| Distinct | `↑distinct` | not linear | `↑H(z^-1(I(d)), d)` (Prop 4.7) | `I(d)` (integrated input) | `distinct`, `hash_distinct`; per-step `stream_distinct` |
| Fixpoint | `δ0 ... (↑T)^Δ ... ∫` with `↑z^-1` back-edge (Eq 5.1, Eq 6.1) | n/a | cycle rule (Prop 3.2) + lifting cycles (Prop 6.2) | nested-time traces, size `Σ_{t2} ‖Σ_{t1} s[t1][t2]‖` (Section 6.2) | `ChildCircuit::recursive`, `recursive_dynamic`, `Circuit::fixedpoint`, `Circuit::iterate`; `delta0` for imports |
| Delay | `z^-1` (outer), `↑z^-1` (inner) | LTI | `(z^-1)^Δ = z^-1` | one value | `delay`, `delay_with_initial_value`, `delay_nested`, `Z1<T>`, `DelayedFeedback` |

Constructs in the paper / crate with no row in the listed IR:

| Construct | Paper cite | `dbsp` item |
|---|---|---|
| `I` (integrate) as an explicit node | Def 2.19 | `integrate`, `integrate_nested`, `accumulate_integrate` |
| `D` (differentiate) as an explicit node | Def 2.17 | `differentiate`, `differentiate_nested` |
| `δ0` (enter nested scope) | Section 5 | `delta0` |
| `∫` (leave nested scope) | Section 5 | implicit in `recursive` export (integral that "exports the result", `operator/recursive.rs` doc) |
| `makeset` (scalar -> singleton Z-set) | Section 7.2 | UNVERIFIED which crate op corresponds |
| flatmap of indexed Z-set | Section 7.4 | `flat_map`, `flat_map_index` |
| outer join, semijoin, asof join, star join, range join | not in paper | `outer_join`, `semijoin_stream`, `asof_join`, `multijoin/star_join.rs`, `join_range` |
| topk / lag / rank / row_number | ORDER BY listed as future work, Section 7.2 | `operator/group/{topk,lag,rank,row_number}.rs` |
| shard / exchange / gather | not in paper | `operator/communication/` |
| stream-relation join `I(s) ↑⋈ t` | Section 7.6 | UNVERIFIED which crate op corresponds (candidate: `stream_join` against an integrated trace) |
