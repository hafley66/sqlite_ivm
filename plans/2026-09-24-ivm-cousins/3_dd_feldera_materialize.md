# differential-dataflow, Feldera SQL compiler, Materialize, DDlog

Source pins (shallow clones read on 2026-09-24; nothing built):

| Repo | Commit | Date | Version |
|---|---|---|---|
| TimelyDataflow/differential-dataflow | `aa8745f9` | 2026-07-29 | `differential-dataflow` 0.25.1 (released 2026-07-15), depends on `timely` 0.31 |
| TimelyDataflow/timely-dataflow | HEAD | 2026-09 | `timely` 0.31.0 |
| feldera/feldera | `be8df0c1` | 2026-09-24 | MIT (open-source edition) |
| MaterializeInc/materialize | `25bb1585` | 2026-09-24 | BSL 1.1 |
| vmware-archive/differential-datalog | `ca5ded82` | 2022-11-29 | last release 1.2.3 (2021-12-13), MIT |

Paths below are relative to each repo root.

---

## 1. differential-dataflow 0.25.x

### 1.1 API shape change: `Collection<G, D, R>` is gone

The `Collection<G: Scope, D, R>` form belongs to releases up to 0.17. CHANGELOG entries:

| Version | Change |
|---|---|
| 0.18.0 (2025-10-23) | `VecCollection` extracted from `Collection` (#651). `Collection` becomes container-generic. |
| 0.23.0 (2026-04-13) | `Arranged<G, Tr>` becomes `Arranged<'scope, Tr>` (#714). `'scope` lifetime added to `VecCollection`, `Arranged` (#718). Scopes passed by value (#720). `S: Scope` generics replaced by the concrete `Scope<'scope, T>` type. |
| 0.24.0 (2026-05-29) | Chunker split out of batcher; `Cursor` gets associated types `Key`, `Val`, `Time`, `Diff`. |
| 0.25.0 (2026-07-15) | Layout moves onto `Cursor`; `join`/`reduce` rebuilt over public `JoinTactic` / `ReduceTactic` traits (experimental); `Chunk` trait (experimental). Changelog says "heavily breaking". |

Current definitions:

```rust
// differential-dataflow/src/collection.rs:24
pub struct Collection<'scope, T: Timestamp, C: 'static> {
    pub inner: Stream<'scope, T, C>,
}
// differential-dataflow/src/collection.rs:353  (module `vec`, re-exported as crate::VecCollection)
pub type Collection<'scope, T, D, R = isize> = super::Collection<'scope, T, Vec<(D, T, R)>>;

// timely/src/dataflow/scope.rs:14,21
pub type Iterative<'scope, TOuter, TInner> = Scope<'scope, Product<TOuter, TInner>>;
pub struct Scope<'scope, T: Timestamp> {
    pub(crate) subgraph: &'scope RefCell<SubgraphBuilder<T>>,
    pub(crate) worker:   &'scope Worker,
}
```

Mapping old -> new generics: `G` (scope type) -> `'scope` lifetime + `T` (timestamp); `D, R` -> container `C = Vec<(D, T, R)>` or the `VecCollection<'scope, T, D, R>` alias.

### 1.2 Traits

| Trait | Definition | Path |
|---|---|---|
| `Data` | `pub trait Data : Ord + Debug + Clone + 'static {}` blanket impl | `src/lib.rs` |
| `ExchangeData` | `pub trait ExchangeData : timely::ExchangeData + Data {}` blanket impl (adds `Send + Sync + serde` via timely) | `src/lib.rs` |
| `Hashable` | key hashing for exchange; required on keys of `arrange_*`, `reduce`, `join` | `src/hashable.rs` |
| `IsZero` | `fn is_zero(&self) -> bool` | `src/difference.rs:14` |
| `Semigroup<Rhs = Self>` | `: Clone + IsZero { fn plus_equals(&mut self, rhs: &Rhs); }` | `src/difference.rs:37` |
| `Monoid` | `: Semigroup { fn zero() -> Self; }` | `src/difference.rs:50` |
| `Abelian` | `: Monoid { fn negate(&mut self); }` (re-exported as `Diff`) | `src/difference.rs:60` |
| `Multiply<Rhs = Self>` | `type Output; fn multiply(self, rhs: &Rhs) -> Self::Output;` (join output diff) | `src/difference.rs:66` |
| `Lattice` | `: PartialOrder { fn join(&self, &Self) -> Self; fn meet(..) ...}`; impls for integers, `Duration`, `()`, `Product<T1,T2>`, `(T1,T2)`, `Antichain<T>` | `src/lattice.rs:11` |
| `Timestamp`, `PartialOrder`, `TotalOrder`, `Refines` | timely traits | `timely/src/progress/timestamp.rs`, `timely/src/order.rs` |
| `Scope` | now a struct, section 1.1 | `timely/src/dataflow/scope.rs:21` |
| `TraceReader`, `Trace`, `BatchReader`, `Batcher`, `Builder`, `Cursor`, `Navigable` | trace layer | `src/trace/mod.rs`, `src/trace/cursor/mod.rs` |

### 1.3 Operators

Receiver is `VecCollection<'scope, T, D, R>` unless noted. Common bounds for keyed ops: `T: Timestamp + Lattice + Ord`, `K: ExchangeData + Hashable`, `V: ExchangeData`, `R: ExchangeData + Semigroup`.

| Op | Signature (abridged) | Arranges? | Path |
|---|---|---|---|
| `map` / `flat_map` / `filter` / `map_in_place` | `fn map<D2, L: FnMut(D)->D2>(self, L) -> VecCollection<'scope,T,D2,R>` | no | `src/collection.rs:371-446` |
| `concat` / `concatenate` | `fn concat(self, other: Self) -> Self` | no | `src/collection.rs:71,98` |
| `negate` | `fn negate(self) -> Self where C: Negate` | no | `src/collection.rs:187` |
| `enter` / `leave` / `enter_region` / `leave_region` / `enter_at` | move between parent and child scope; `leave` needs `T: Refines<TOuter>` | no | `src/collection.rs:111,217,290,309,544` |
| `inspect` / `inspect_batch` / `inspect_container` | `fn inspect<F: FnMut(&(D,T,R))>(self, F) -> Self` | no | `src/collection.rs:602,628,134` |
| `probe` / `probe_with` | `fn probe(self) -> (probe::Handle<T>, Self)` | no | `src/collection.rs:146,156` |
| `arrange_by_key` | `(K,V)` input; `-> Arranged<'scope, TraceAgent<ValSpine<K,V,T,R>>>` | yes (is the arrangement) | `src/collection.rs:1072` |
| `arrange_by_self` | `K` input; `-> Arranged<'scope, TraceAgent<KeySpine<K,T,R>>>` | yes | `src/collection.rs:1091` |
| `arrange_core` | `fn arrange_core<'scope,P,C,Chu,Ba,Bu,Tr>(stream, pact, name) -> Arranged<'scope, TraceAgent<Tr>>` | yes | `src/operators/arrange/arrangement.rs:352` |
| `join` | `fn join<V2,R2>(self, other: VecCollection<'scope,T,(K,V2),R2>) -> VecCollection<'scope,T,(K,(V,V2)),<R as Multiply<R2>>::Output>` | both inputs `arrange_by_key` | `src/collection.rs:1128` |
| `join_map` | as `join` plus `logic: FnMut(&K,&V,&V2)->D` | both inputs | `src/collection.rs:1155` |
| `semijoin` | `other: VecCollection<'scope,T,K,R2>`; left `arrange_by_key`, right `arrange_by_self` | both | `src/collection.rs:1183` |
| `antijoin` | `self.concat(self.semijoin(other).negate())`; output diff `R` | via semijoin | `src/collection.rs:1215` |
| `join_core` (collection) | `fn join_core<Tr2,I,L,R2>(self, stream2: Arranged<'scope,Tr2>, result: L) -> VecCollection<'scope,T,I::Item,<R as Multiply<R2>>::Output>` where `L: FnMut(&K,&V,BatchVal<'_,Tr2>)->I` | self arranged; other pre-arranged | `src/collection.rs:1245` |
| `join_core` (arranged) | `fn join_core<Tr2,I,L,R1,R2,KC>(self, other: Arranged<'scope,Tr2>, result: L) -> VecCollection<...>`; `L: FnMut(KC::ReadItem<'_>, BatchVal<'_,Tr1>, BatchVal<'_,Tr2>) -> I` | both pre-arranged | `src/operators/arrange/arrangement.rs:230` |
| `join_traces` | lower-level: emits `Stream<'scope, T, CB::Container>` | both pre-arranged | `src/operators/join.rs:63` |
| `reduce` | `fn reduce<L, V2: Data, R2: Ord+Abelian+'static>(self, logic: L) -> VecCollection<'scope,T,(K,V2),R2>` where `L: FnMut(&K, &[(&V, R)], &mut Vec<(V2, R2)>)` | `arrange_by_key` input + output trace | `src/collection.rs:741` |
| `reduce_abelian` | `fn reduce_abelian<L,Bu,T2>(self, name, logic) -> Arranged<'scope, TraceAgent<T2>>`; output `Diff: Abelian` | yes | `src/collection.rs:784`; arranged form `src/operators/arrange/arrangement.rs:269` |
| `reduce_core` | `L: FnMut(&K, &[(&V,R)], &mut Vec<(V,Diff)> /*prior output*/, &mut Vec<(V,Diff)> /*change*/)` | yes | `src/collection.rs:803`; `arrangement.rs:290`; driver `src/operators/reduce.rs:93` (`reduce_trace`), `:120` (`reduce_with_tactic`) |
| `count` | `fn count(self) -> VecCollection<'scope,T,(K,R),isize>`; `count_core<R2: Ord+Abelian+From<i8>>` | `arrange_by_self` + reduce | `src/collection.rs:912,919` |
| `count_total` | trait `CountTotal`, requires `T: TotalOrder`; no output trace | `arrange_by_self` | `src/operators/count.rs:17` |
| `distinct` | `fn distinct(self) -> VecCollection<'scope,T,K,isize>` = `threshold_named("Distinct", |_,_| 1)` | `arrange_by_self` + reduce | `src/collection.rs:841,850` |
| `threshold` | `fn threshold<R2: Ord+Abelian, F: FnMut(&K,&R1)->R2>(self, F) -> VecCollection<'scope,T,K,R2>` | `arrange_by_self` + reduce | `src/collection.rs:872` |
| `threshold_total` | trait `ThresholdTotal`, requires `T: TotalOrder` | `arrange_by_self` | `src/operators/threshold.rs:20` |
| `consolidate` | `fn consolidate(self) -> Self` (via `KeyBatcher`/`KeySpine`) | yes (transient arrange) | `src/collection.rs:959,966` |
| `consolidate_stream` | per-batch consolidation, `Pipeline` pact, no trace | no | `src/collection.rs:1004` |
| `iterate` | `fn iterate<F>(self, logic: F) -> VecCollection<'scope,T,D,R> where for<'inner> F: FnOnce(Iterative<'inner,T,u64>, VecCollection<'inner,Product<T,u64>,D,R>) -> VecCollection<'inner,Product<T,u64>,D,R>`; impl for collections needs `R: Abelian`, impl for `Scope` needs `R: Semigroup` | no (feedback edge) | `src/operators/iterate.rs:49,81,102` |
| `Variable` | `pub struct Variable<'scope, T: Timestamp+Lattice, C: Container>`; `fn new(scope, step: T::Summary) -> (Self, Collection)`; `fn new_from(source, step) -> (Self, Collection)` (needs `Negate`); `fn set(self, result: Collection)` | no | `src/operators/iterate.rs:192,221,252,262` |
| `SemigroupVariable` | removed in 0.20.0 (#674); `Variable::new` does not require `Negate` and replaces it | n/a | `CHANGELOG.md:152-167` |
| `results_in` | advances timestamps by a summary (used by `Variable::set`) | no | `src/collection.rs:251` |

`Arranged` and `TraceAgent`:

```rust
// src/operators/arrange/arrangement.rs:45
pub struct Arranged<'scope, Tr: TraceReader> {
    pub stream: Stream<'scope, Tr::Time, Vec<Tr::Batch>>,
    pub trace: Tr,
}
// methods: enter, enter_region, enter_at, as_collection, as_vecs, flat_map_ref,
//          join_core, reduce_abelian, reduce_core, leave_region

// src/operators/arrange/agent.rs:27
pub struct TraceAgent<Tr: TraceReader> { .. }   // no 'scope lifetime
// import(&mut self, scope: Scope<'scope, Tr::Time>) -> Arranged<'scope, TraceAgent<Tr>>   (:211)
// import_core -> (Arranged, ShutdownButton<CapabilitySet<Tr::Time>>)                       (:270)
// import_frontier / import_frontier_core -> Arranged<'scope, TraceFrontier<TraceAgent<Tr>>> (:383,:400)
```

Trace implementations: `ValSpine`, `KeySpine`, `ValBatcher`, `KeyBatcher`, `ValBuilder`, `KeyBuilder` in `src/trace/implementations/ord_neu.rs`, `src/trace/implementations/mod.rs`; merge scheduling in `src/trace/implementations/spine_fueled.rs`.

### 1.4 Building and feeding a dataflow

Timely entry points:

```rust
// timely/src/worker.rs:627
pub fn dataflow<T, R, F>(&mut self, func: F) -> R
where T: Refines<()>, F: FnOnce(Scope<T>) -> R;
// :650 dataflow_named(name, func)   :273 step(&mut self) -> bool   :445 step_while<F: FnMut()->bool>(&mut self, F)
```

DD input:

```rust
// src/input.rs:19
pub trait Input<'scope> : TimelyInput<'scope> {
    fn new_collection<D: Data, R: Semigroup+'static>(&self)
        -> (InputSession<Self::Timestamp, D, R>, VecCollection<'scope, Self::Timestamp, D, R>);
    // also new_collection_from(iter)
}
// impl for Scope<'scope, T> where T: Timestamp + Lattice + TotalOrder   (src/input.rs:101)

// src/input.rs:172
pub struct InputSession<T: Timestamp+Clone, D: Data, R: Semigroup+'static> { .. }  // no 'scope lifetime
// insert(D) / remove(D)            (R = isize only)        :180,182
// update(D, R) / update_at(D, T, R)                         :217,229
// flush()   sends buffer, advances timely input handle      :246
// advance_to(T)  asserts monotone; does not notify timely until flush/drop  :258
// time() / epoch()   close(self)   Drop flushes             :265-273
// to_collection(scope) / from(Handle<T,(D,T,R)>)            :188,208
```

Canonical driver loop (`differential-dataflow/examples/hello.rs`):

```rust
timely::execute_from_args(args, move |worker| {
    let (mut input, probe) = worker.dataflow::<u32,_,_>(|scope| {
        let (input, edges) = scope.new_collection::<_, i32>();
        let (probe, _) = edges.map(..).count_total().inspect(|x| println!("{:?}", x)).probe();
        (input, probe)            // only 'scope-free handles leave the closure
    });
    input.update(rec, 1);
    input.advance_to(1);
    input.flush();
    worker.step_while(|| probe.less_than(input.time()));
});
```

Output delta read paths:

| Mechanism | Shape | Path |
|---|---|---|
| `inspect` / `inspect_batch` | callback per `(D, T, R)` or per batch inside the dataflow | `src/collection.rs:602,628` |
| `probe` + `ProbeHandle::less_than(&T)` | frontier signal; caller steps the worker until the probe passes the input time | `src/collection.rs:146` |
| timely `Capture::capture()` on `collection.inner` | `std::sync::mpsc::Receiver<Event<T, C>>`; `Extract::extract()` -> `Vec<(T, C)>` after the worker completes | `timely/src/dataflow/operators/core/capture/capture.rs:111`, `.../capture/extract.rs:50`; used in `differential-dataflow/tests/join.rs:21-24` |
| DD CDC v2 capture | `Message<D,T,R> = Updates(Vec<(D,T,R)>) | Progress(Progress<T>)`, `Writer<T>` sink trait, dedup/reorder iterator | `src/capture.rs:15-70` |
| `TraceAgent` export + `Cursor` | arrangement handle survives the dataflow closure; read by cursor or `import` into another dataflow | `src/operators/arrange/agent.rs` |

Lifetime constraints:

| Item | Lifetime-bound? | Consequence |
|---|---|---|
| `Scope<'scope, T>` | yes, borrows `&'scope RefCell<SubgraphBuilder>` and `&'scope Worker` | exists only during the closure |
| `Collection` / `VecCollection` / `Arranged` / `Stream` / `Variable` | carry `'scope` | `F: FnOnce(Scope<T>) -> R` has an elided (higher-ranked) lifetime, so `R` cannot name `'scope`; collections cannot be returned from `worker.dataflow` |
| iterate closure | `for<'inner> F: FnOnce(Iterative<'inner,..>, VecCollection<'inner,..>) -> VecCollection<'inner,..>` | inner collections cannot escape the iterative scope except through `leave` |
| `InputSession`, `ProbeHandle`, `TraceAgent`, `ShutdownButton`, capture `Receiver` | no `'scope` | these are what a caller returns from the closure and keeps |

Interpretation for a trait over DD and a second engine: the DD-side handle type the caller can own across steps is `(InputSession, ProbeHandle, TraceAgent | Receiver)`; `Collection` values exist only inside a builder callback. UNVERIFIED whether the same constraint held pre-0.23 in type form (it held at runtime because `Child<'a, ..>` carried a lifetime; DDlog's `TransformerMap<'a>` uses `Child<'a, Worker<Allocator>, TS>`, section 5).

---

## 2. Feldera SQL compiler (`sql-to-dbsp-compiler`)

License: `LICENSE` header "OPEN-SOURCE EDITION - MIT LICENSE", "Copyright 2021-2023 VMware, Inc." for DBSP, with a separate Feldera Enterprise carve-out.

Root: `sql-to-dbsp-compiler/SQL-compiler/src/main/java/org/dbsp/sqlCompiler/` (abbreviated `$C`). Runtime: Rust crate `crates/dbsp`; SQL runtime library `crates/sqllib`.

### 2.1 Pipeline stages

| # | Stage | Class | Path |
|---|---|---|---|
| 0 | Driver | `DBSPCompiler` (holds `sqlToRelCompiler`, `relToDBSPCompiler`; `optimize()` at line 901) | `$C/compiler/DBSPCompiler.java:185-186,901` |
| 1 | Parse + validate + SqlNode -> RelNode (Calcite) | `SqlToRelCompiler`, `FelderaSqlToRelConverter`, `CteToLocalViews`, `Catalog` | `$C/compiler/frontend/calciteCompiler/` |
| 2 | Calcite rule-based logical optimization | `CalciteOptimizer` + custom rules (`InnerDecorrelator`, `ExceptOptimizerRule`, `SetopOptimizerRule`, `RowsToRangeRule`, `SessionRewriteRule`, `AntiJoinDistinctRemoveRule`, ...) | `$C/compiler/frontend/calciteCompiler/optimizer/` |
| 3 | RelNode -> non-incremental DBSP circuit ("outer" IR) | `CalciteToDBSPCompiler` (3448 lines; one `visitX` per `Logical*` RelNode), `ExpressionCompiler` (RexNode -> inner IR), `TypeCompiler`, `aggregates/*` | `$C/compiler/frontend/` |
| 4 | Circuit passes incl. incrementalization | `CircuitOptimizer` (ordered pass list, lines 82-205) | `$C/compiler/visitors/outer/CircuitOptimizer.java` |
| 5 | Code generation | `ToRustVisitor` (outer), `ToRustInnerVisitor` (expressions), `RustFileWriter`, `multi/` (multi-crate output) | `$C/compiler/backend/rust/` |
| alt | Other backends | `ToJsonOuterVisitor`, `ToJsonInnerVisitor`, `ToSqlVisitor`, `dot/`, `MerkleOuter` | `$C/compiler/backend/` |

IR layers:

| Layer | Root types | Path |
|---|---|---|
| Outer (circuit) | `DBSPCircuit`, `DBSPOperator` (base, `inputs: List<OutputPort>`, `annotations`), `DBSPSimpleOperator` (single output; field `operation` = Rust `Stream` method name), `DBSPUnaryOperator`, `DBSPBinaryOperator`, `DBSPNestedOperator`, `OutputPort` | `$C/circuit/`, `$C/circuit/operator/` |
| Inner (expressions/types) | `DBSPExpression`, `DBSPType`, `DBSPFunction`, `DBSPStatement`, aggregate IR | `$C/ir/expression`, `$C/ir/type`, `$C/ir/statement`, `$C/ir/aggregate` |
| Stream kind | `StreamKind.DELTA` vs whole-collection; checked by `ValidateStreamKinds` | `$C/compiler/visitors/outer/ValidateStreamKinds.java` |

Naming rule from `DBSPSimpleOperator` javadoc: classes with `Stream` in the name map to Rust `stream_*` methods (non-incremental, per-step); the name without `Stream` is the incremental operator (e.g. `DBSPStreamDistinctOperator` operation `"stream_distinct"` vs `DBSPDistinctOperator` operation `"distinct"`; `DBSPStreamAggregateOperator` `"stream_aggregate"` vs `DBSPAggregateOperator` `"aggregate"`).

### 2.2 Operator classes (`$C/circuit/operator/`, 94 files)

| Group | Classes |
|---|---|
| Linear / stateless | `DBSPMapOperator`, `DBSPMapIndexOperator`, `DBSPFilterOperator`, `DBSPFlatMapOperator`, `DBSPFlatMapIndexOperator`, `DBSPDeindexOperator`, `DBSPNegateOperator`, `DBSPSumOperator`, `DBSPSubtractOperator`, `DBSPNoopOperator`, `DBSPHopOperator` (desugared to map+flat_map), `DBSPPositiveOperator` |
| Calculus | `DBSPIntegrateOperator` (`integrate`), `DBSPDifferentiateOperator`, `DBSPDelayOperator` (z^-1), `DBSPDelayedIntegralOperator`, `DBSPDeltaOperator` (delta0), `DBSPUpsertFeedbackOperator` |
| Joins | `DBSPStreamJoinOperator`, `DBSPStreamJoinIndexOperator`, `DBSPStreamAntiJoinOperator`, `DBSPJoinOperator`, `DBSPJoinIndexOperator`, `DBSPJoinFilterMapOperator`, `DBSPAntiJoinOperator`, `DBSPLeftJoinOperator`, `DBSPLeftJoinIndexOperator`, `DBSPLeftJoinFilterMapOperator`, `DBSPStarJoinOperator` (N-ary, incremental-only), `DBSPStarJoinIndexOperator`, `DBSPStarJoinFilterMapOperator`, `DBSPAsofJoinOperator` -> `DBSPConcreteAsofJoinOperator` |
| Distinct | `DBSPStreamDistinctOperator`, `DBSPDistinctOperator`, `DBSPBinaryDistinctOperator` |
| Aggregation | `DBSPStreamAggregateOperator`, `DBSPAggregateOperator`, `DBSPAggregateLinearPostprocessOperator`, `DBSPAggregateLinearPostprocessRetainKeysOperator`, `DBSPAggregateZeroOperator`, `DBSPChainAggregateOperator` (min/max on append-only, O(1)), `DBSPPrimitiveAggregateOperator`, `DBSPWeighOperator` |
| Window / ordering | `DBSPPartitionedRollingAggregateOperator` (delta-only), `DBSPPartitionedRollingAggregateWithWaterlineOperator`, `DBSPLagOperator` (LAG/LEAD), `DBSPIndexedTopKOperator`, `DBSPRankOperator`, `DBSPRowNumberOperator`, `DBSPWindowOperator` (range filter by scalar pair), `DBSPWaterlineOperator`, `DBSPInputMapWithWaterlineOperator`, `DBSPControlledKeyFilterOperator` |
| GC / state retention | `DBSPIntegrateTraceRetainKeysOperator`, `DBSPIntegrateTraceRetainValuesOperator`, `DBSPIntegrateTraceRetainNValuesOperator` |
| Sources / sinks / views | `DBSPSourceTableOperator`, `DBSPSourceMultisetOperator`, `DBSPSourceMapOperator`, `DBSPConstantOperator`, `DBSPNowOperator`, `DBSPSinkOperator`, `DBSPViewOperator`, `DBSPViewDeclarationOperator` (recursive view decl; "behaves exactly like a delay operator that closes a cycle") |
| Structural | `DBSPNestedOperator` (container for a recursive SCC), `DBSPChainOperator`, `DBSPApplyOperator`, `DBSPApply2Operator`, `DBSPApplyNOperator`, `DBSPInternOperator`, `DBSPWeightValidatorOperator` |
| Marker interfaces | `ILinear`, `IIncremental`, `INonIncremental`, `IStateful`, `IJoin`, `IGCOperator`, `IContainsIntegrator`, `IHasInputIntegrator`, `IHasPostIntegrator`, `ILinearAggregate`, `INonLinearAggregate`, `IMultiOutput`, `IInputOperator` |

### 2.3 RelNode -> operators (stage 3, counts of `new DBSP*Operator` inside each visit method of `CalciteToDBSPCompiler.java`)

| Calcite node | Method (line) | Operators emitted |
|---|---|---|
| `TableScan` | `visitScan` (1022) | source operator |
| `LogicalProject` | `visitProject` (1068) | `Map` |
| `LogicalFilter` | `visitFilter` (1259) | `Filter` |
| `LogicalUnion` | `visitUnion` (1189) | `Sum` x2, `StreamDistinct` (UNION vs UNION ALL) |
| `LogicalMinus` | `visitMinus` (1213) | `Negate`, `Sum`, `StreamDistinct`, `Positive`, `Integrate`, `Differentiate` |
| `LogicalIntersect` | `visitIntersect` (2323) | delegates |
| `LogicalJoin` | `visitJoin` (1421) | `MapIndex` x10, `StreamJoin`, `StreamAntiJoin` x4, `LeftJoin`, `Filter`, `Map`, `Sum`, `Integrate`, `Differentiate` (outer joins built from join + antijoin + null padding) |
| `LogicalAsofJoin` | `visitAsofJoin` (1914) | `AsofJoin` |
| `LogicalAggregate` | `visitAggregate` (983) | `StreamAggregate` (line 802), `AggregateZero` (975) for global aggregate on empty input, `Map`, `Sum`, `StreamDistinct` |
| `LogicalWindow` (OVER) | `visitWindow` (2543) | delegates to `aggregates/WindowAggregates` (`MapIndex`, `Map`), `RangeAggregates` (`PartitionedRollingAggregate`, `StreamJoin`, `MapIndex` x3, `Integrate`, `Differentiate`), `LeadLagAggregates` (`Lag`, `Deindex`, `Integrate`, `Differentiate`), `RankAggregate` (`IndexedTopK`, `Rank`, `RowNumber`, `Integrate` x2, `Differentiate` x2) |
| `LogicalSort` (ORDER BY/LIMIT) | `visitSort` (2680) | `IndexedTopK` x2, `StreamAggregate`, `MapIndex`, `Map`, `Deindex`, `Subtract` x2, `Integrate` x2, `Differentiate` |
| `LogicalCorrelate` / `Uncollect` | `visitCorrelate` (411), `visitUncollect` (711) | `FlatMap`, `Filter` |
| `LogicalValues` | `visitLogicalValues` (2271) | `Constant` |

### 2.4 Incrementalization

Stage 3 builds a circuit over whole collections using `Stream*` operators. Pass `IncrementalizeVisitor` (`$C/compiler/visitors/outer/IncrementalizeVisitor.java`) rewrites the boundary only:

| Node | Rewrite |
|---|---|
| source | source switched to `StreamKind.DELTA`, followed by `DBSPIntegrateOperator` |
| `DBSPConstantOperator`, `DBSPNowOperator` | `Differentiate` then `Integrate` |
| `DBSPSinkOperator` | `Differentiate` inserted before sink |

`OptimizeIncrementalVisitor` (`$C/compiler/visitors/outer/OptimizeIncrementalVisitor.java`, "pushing integral operators forward") then applies DBSP chain-rule rewrites:

| Operator | Rule |
|---|---|
| `Map`, `MapIndex`, `Filter`, `Negate`, `FlatMap`, `Deindex`, `Noop`, `Hop`, `View`, `PartitionedRollingAggregate`, `ChainAggregate` | linear: `op(I(x)) = I(op(x))`, integral moved after |
| `Differentiate` after `Integrate` | cancelled (`D(I(x)) = x`), unless annotated `NoInc` |
| `StreamJoin(I(a), I(b))` | `I(Join(a, b))` with incremental `DBSPJoinOperator` |
| `StreamJoinIndex`, `StreamAntiJoin` | same pattern -> `JoinIndex`, `AntiJoin` |
| `Sum`, `AtomicSum`, `Subtract` over all-integrated inputs | operator on deltas, then `Integrate` |
| `StreamDistinct(I(x))` | `I(Distinct(x))` |
| `StreamAggregate(I(x))` | `I(Aggregate(x))` |
| `DBSPNestedOperator` | integrators from inputs pushed to outputs; `DBSPDeltaOperator` handled with nested resurfacing |

Remaining passes of note (order from `CircuitOptimizer.java`): `RecursiveComponents` (91), `ExpandAggregates` (110; splits `StreamAggregate` by append-only-ness into `AggregateLinearPostprocess`, `ChainAggregate`, `StreamAggregate`), `OptimizeDistinctVisitor` (115), `IncrementalizeVisitor` (120), `OptimizeIncrementalVisitor` (117, 122), `RemoveIAfterD` (123), `ShareIndexes` (133), `ShareWindowIntegrals` (138), `MonotoneAnalyzer` (144; waterline/GC insertion from `monotonicity/`), `FindUnboundedState` (153), `ExpandHop` (162), `LowerAsof` (176), `LowerCircuitVisitor` (177), `BalancedJoins` (179), `ImplementJoins` (185), `PushDifferentialsUp` (187), `CSE` (multiple).

### 2.5 Recursion

`WITH RECURSIVE`-style mutually recursive views are declared with `DECLARE RECURSIVE VIEW`; stage 3 records them in a map "Recursive views, indexed by actual view name" (`CalciteToDBSPCompiler.java:260`) and emits `DBSPViewDeclarationOperator` (line 3246). UNVERIFIED: exact SQL syntax string `DECLARE RECURSIVE VIEW` (taken from Feldera docs memory, not from the sources read).

`RecursiveComponents` javadoc (`$C/compiler/visitors/outer/recursive/RecursiveComponents.java`):
1. compute SCCs
2. normalize connections inside SCCs
3. group SCC nodes into `DBSPNestedOperator` (`BuildNestedOperators`), annotated `Recursive`
4. rewrite `LeftJoin` into join + antijoin inside recursive components (`SubstituteLeftJoins`)
5. validate (`ValidateRecursiveOperators`)

Operators rejected inside a recursive component (`ValidateRecursiveOperators.java`): `Apply`, `Apply2`, `IndexedTopK`, `Rank`, `RowNumber`, `IntegrateTraceRetainKeys/Values/NValues` (GC), `Lag`, nested `Nested`, `Now`, `SourceBase` inputs, `Waterline`, `Window`, `PartitionedRollingAggregate(WithWaterline)` (OVER). `DBSPViewDeclarationOperator` is explicitly allowed. Joins, aggregates, distinct, antijoin are allowed (no reject rule).

Codegen: `ToRustVisitor.preorder(DBSPNestedOperator)` (`$C/compiler/backend/rust/ToRustVisitor.java:427-470`) emits `let (outs..) = circuit.recursive(|circuit, (decls..): (stream types..)| { .. })`, i.e. DBSP's `recursive` nested-circuit fixpoint.

### 2.6 Window and aggregate handling summary

| SQL construct | DBSP lowering |
|---|---|
| `GROUP BY` aggregate | `StreamAggregate` -> incremental `Aggregate`; linear aggregates (SUM/COUNT) -> `AggregateLinearPostprocess`; MIN/MAX on append-only input -> `ChainAggregate`; global aggregate with empty input -> `AggregateZero` |
| `OVER (... RANGE/ROWS ...)` rolling aggregate | `PartitionedRollingAggregate` (operates on deltas; `Differentiate` before, `Integrate` after), joined back to rows via `StreamJoin` |
| `LAG`/`LEAD` | `DBSPLagOperator` |
| `RANK`/`ROW_NUMBER`/`ORDER BY ... LIMIT` | `IndexedTopK`, `Rank`, `RowNumber` |
| `TUMBLE`/`HOP` | `Hop` -> map + flat_map |
| `NOW()` temporal filters | `ImplementNow`, `temporal/` passes, `Waterline` + `IntegrateTraceRetain*` for GC |

---

## 3. Materialize

### 3.1 License (`LICENSE`, read 2026-09-24)

| Field | Value |
|---|---|
| License | Business Source License 1.1 |
| Licensed Work | "Materialize Version 20260924" (the version string is the build date; each version carries its own Change Date) |
| Additional Use Grant | single installation: sum of cluster memory limits < 24 GiB and disk limits < 48 GiB; multiple installations allowed for distinct applications; no sharding one application across installations to exceed limits; no use as a "Database Service" (third parties creating views whose definitions they control) |
| Change Date | September 24, 2030 for this version (commit date + 4 years) |
| Change License | Apache License 2.0 |
| Conversion rule | on the Change Date or the fourth anniversary of first public distribution of that version, whichever comes first |

### 3.2 Lowering pipeline

| Stage | IR | Key type | Path |
|---|---|---|---|
| SQL AST -> HIR | High-level IR, correlated subqueries allowed | `HirRelationExpr`, `HirScalarExpr` | `src/sql/src/plan/hir.rs:109`; planning in `src/sql/src/plan/query.rs`; HIR rewrites `src/sql/src/plan/transform_hir.rs` |
| HIR -> MIR | decorrelation + lowering | `HirRelationExpr::lower(..)` (`lowering.rs:188`), `lower_uncorrelated` (`:1689`), `applied_to` | `src/sql/src/plan/lowering.rs`, `src/sql/src/plan/lowering/variadic_left.rs` |
| MIR optimization | `Optimizer::logical_optimizer` (752), `physical_optimizer` (822), `logical_cleanup_pass` (939), `fast_path_optimizer` (987); join planning in `join_implementation.rs` | `MirRelationExpr`, `MirScalarExpr` | `src/transform/src/lib.rs`, `src/transform/src/*.rs` |
| MIR -> LIR | physical plan with arrangement choices | `Context::lower` (`plan/lowering.rs:136`), `finalize_dataflow` (`plan.rs:668`) | `src/compute-types/src/plan/lowering.rs`, `src/compute-types/src/plan.rs` |
| LIR -> DD | rendering into timely/differential operators | `LirRelationExpr` | `src/compute/src/render/` (`join.rs`, `join/`, `reduce.rs`, `threshold.rs`, `top_k.rs`, `flat_map.rs`, `context.rs`) |

HIR variants (`src/sql/src/plan/hir.rs:109`): `Constant`, `Get`, `LetRec`, `Let`, `Project`, `Map`, `CallTable`, `Filter`, `Join`, `Reduce`, `Distinct`, `TopK`, `Negate`, `Threshold`, `Union`. HIR `Distinct` lowers to MIR `.distinct()` (`lowering.rs:880`) = `distinct_by(0..arity)` (`relation.rs:1373`), i.e. a `Reduce` with no aggregates (UNVERIFIED last step; `distinct_by` body not read).

### 3.3 `MirRelationExpr` (`src/expr/src/relation.rs:99`)

Doc comment: "The AST is meant to reflect the capabilities of the `differential_dataflow::Collection` type".

| Variant | Fields |
|---|---|
| `Constant` | `rows: Result<Vec<(Row, Diff)>, EvalError>`, `typ: ReprRelationType` |
| `Get` | `id: Id`, `typ: ReprRelationType`, `access_strategy: AccessStrategy` |
| `Let` | `id: LocalId`, `value: Box<MirRelationExpr>`, `body: Box<MirRelationExpr>` |
| `LetRec` | `ids: Vec<LocalId>`, `values: Vec<MirRelationExpr>`, `limits: Vec<Option<LetRecLimit>>`, `body: Box<MirRelationExpr>` |
| `Project` | `input: Box<MirRelationExpr>`, `outputs: Vec<usize>` |
| `Map` | `input`, `scalars: Vec<MirScalarExpr>` |
| `FlatMap` | `input`, `func: TableFunc`, `exprs: Vec<MirScalarExpr>` |
| `Filter` | `input`, `predicates: Vec<MirScalarExpr>` |
| `Join` | `inputs: Vec<MirRelationExpr>`, `equivalences: Vec<Vec<MirScalarExpr>>`, `implementation: JoinImplementation` |
| `Reduce` | `input`, `group_key: Vec<MirScalarExpr>`, `aggregates: Vec<AggregateExpr>`, `monotonic: bool`, `expected_group_size: Option<u64>` |
| `TopK` | `input`, `group_key: Vec<usize>`, `order_key: Vec<ColumnOrder>`, `limit: Option<MirScalarExpr>`, `offset: usize`, `monotonic: bool`, `expected_group_size: Option<u64>` |
| `Negate` | `input` |
| `Threshold` | `input` |
| `Union` | `base: Box<MirRelationExpr>`, `inputs: Vec<MirRelationExpr>` |
| `ArrangeBy` | `input`, `keys: Vec<Vec<MirScalarExpr>>` |

`JoinImplementation` variants: `Differential(..)` (linear binary join chain), `DeltaQuery(Vec<Vec<(usize, Vec<MirScalarExpr>, Option<JoinInputCharacteristics>)>>)`, `IndexedFilter(..)`, `Unimplemented` (`relation.rs` ~3223).

Recursion: `LetRec` (from SQL `WITH MUTUALLY RECURSIVE`, UNVERIFIED syntax name from memory) with per-binding iteration `limits`.

### 3.4 LIR (`src/compute-types/src/plan.rs:295,304`)

`LirRelationExpr` wraps `LirRelationNode` plus a dataflow-local id. Variants:

| Variant | Fields |
|---|---|
| `Constant` | `rows: Result<Vec<(StableRow, Timestamp, Diff)>, EvalError>` |
| `Get` | `id: Id`, `keys: AvailableCollections`, `plan: GetPlan` |
| `Let` | `id`, `value`, `body` |
| `LetRec` | `ids`, `values`, `limits`, `body` |
| `Mfp` | `input`, `mfp: MfpPlan<LirScalarExpr>`, `input_key_val` (Map/Filter/Project fused) |
| `FlatMap` | `input_key`, `input`, `exprs`, `func: TableFunc`, `mfp_after` |
| `Join` | `inputs`, `plan: JoinPlan` (`Linear(LinearJoinPlan)` / `Delta(DeltaJoinPlan)`, `plan/join.rs:48`) |
| `Reduce` | `input_key`, `input`, `key_val_plan: KeyValPlan`, `plan: ReducePlan` (`Distinct` / `Accumulable` / `Hierarchical` / `Basic`, `plan/reduce.rs:133`), `mfp_after`, `temporal_bucketing_strategy` |
| `TopK` | `input`, `top_k_plan: TopKPlan` (`MonotonicTop1` / `MonotonicTopK` / `Basic`, `plan/top_k.rs:30`), `temporal_bucketing_strategy` |
| `Negate` | `input` |
| `Threshold` | `input`, `threshold_plan: ThresholdPlan` (`Basic`, `plan/threshold.rs:36`) |
| `Union` | `inputs`, `consolidate_output: bool`, `temporal_bucketing_strategies` |
| `ArrangeBy` | `input_key`, `input`, `input_mfp`, `forms: AvailableCollections`, `strategy: ArrangementStrategy` |

MIR -> LIR deltas: `Project`/`Map`/`Filter` fuse into `Mfp`; `ArrangeBy.keys` becomes `forms: AvailableCollections`; `Reduce`/`TopK`/`Threshold`/`Join` gain a chosen physical plan enum.

---

## 4. DDlog (vmware/differential-datalog)

| Item | Value |
|---|---|
| Repo | `github.com/vmware-archive/differential-datalog` (moved from `vmware/`) |
| GitHub status | `archived: true`, last push 2023-07-07, last commit 2022-11-29 |
| Last release | 1.2.3, 2021-12-13 (`CHANGELOG.md`) |
| License | MIT |
| Compiler language | Haskell (`src/Language/DifferentialDatalog/*.hs`, `package.yaml`, `stack.yaml`) |
| DD dependency | forked: `differential-dataflow`, `dogsdogsdogs`, `timely` from `github.com/ddlog-dev/*` branch `ddlog-4` (`rust/template/differential_datalog/Cargo.toml:17-20`) |
| Relation to Feldera | Feldera `LICENSE` carries "Database Stream Processor ... Copyright 2021-2023 VMware, Inc."; UNVERIFIED: DDlog authors (Ryzhyk, Budiu) moved to DBSP/Feldera |

### 4.1 Compilation scheme

1. `ddlog -i prog.dl` (Haskell): parse (`Parse.hs`), type inference (`TypeInference.hs`, `Unification.hs`), validate, optimize (`Optimize.hs`), compile rules (`Compile.hs`, `Rule.hs`, `Index.hs`), split output into crates (`Crate.hs`: "Decompose generated Rust project into crates"; one crate per module without circular dependencies, crates prefixed `types__`).
2. Output: a Cargo workspace (`<prog>_ddlog/`) with a `types` crate (user types, functions, extern Rust) and a main crate, linking the runtime template `rust/template/` (`differential_datalog`, `ddlog_derive`, `cmd_parser`, `ddlog_profiler`, `ovsdb`, C header `ddlog.h`).
3. `cargo build --release` compiles it.

The generated code does not emit typed DD operator chains per rule. It emits a data description of the program that a generic runtime interprets into DD at startup (`rust/template/differential_datalog/src/program/mod.rs`):

| Type | Shape | Line |
|---|---|---|
| `Program` | `{ nodes: Vec<ProgNode>, delayed_rels: Vec<DelayedRelation>, init_data: Vec<(RelId, DDValue)> }` | 269 |
| `ProgNode` | `Rel { rel: Relation }` / `Apply { transformer, source_pos, tfun: TransformerFunc }` / `Scc { rels: Vec<RecursiveRelation> }` | 292 |
| `RecursiveRelation` | `{ rel: Relation, distinct: bool }`; `distinct` applied before closing the loop | 312 |
| `Rule` | `CollectionRule` / `ArrangementRule` | 783 |
| `XFormCollection` | `Arrange`, `Differentiate`, `Map`, `FlatMap`, `Filter`, `FilterMap`, `Inspect`, `StreamJoin`, `StreamSemijoin`, `StreamXForm` | 615 |
| `XFormArrangement` | `FlatMap`, `FilterMap`, `Aggregate`, `Join`, `Semijoin`, `Antijoin`, `StreamJoin`, `StreamSemijoin` | 454 |
| `Arrangement` | `Map` / `Set` | 819 |
| `AggFunc` | `fn(&DDValue, &[(&DDValue, Weight)]) -> Option<DDValue>` | 256 |
| `TransformerMap<'a>` | `FnvHashMap<RelId, Collection<Child<'a, Worker<Allocator>, TS>, DDValue, Weight>>` (hand-written DD fragments, top scope only) | ~275 |
| `DDValue` | `{ val: DDVal, vtable: &'static DDValMethods }`: every DD collection carries this type-erased value | `src/ddval/ddvalue.rs:16` |

Consequence: all DD collections are `Collection<_, DDValue, Weight>`, so DD generic operators are monomorphized once per runtime rather than once per relation type; per-rule logic lives in generated `fn` pointers / closures over `DDValue`. Recursive SCCs map to nested scopes (`ProgNode::Scc`).

### 4.2 Known compile-time costs

| Source | Statement |
|---|---|
| `doc/tutorial/tutorial.md` "Compilation speed" (~line 170) | "Compiling the Rust project generated by DDlog can take a long time." Mitigations: split program into modules (one crate per module, faster re-compilation); keep `playpen_ddlog/` artifacts; `CARGO_PROFILE_RELEASE_OPT_LEVEL="z"` speeds compilation while "generating code that is 50% slower"; debug builds faster but "very large and slow binaries" |
| `CHANGELOG.md` 1.1.0 | `AnyDeserialize` impl feature-gated because it "can cause significant code bloat and slow down compilation" |
| `CHANGELOG.md` 1.2.3 | "Avoid recompiling Rust crates when only arrangement debug info changes" |
| `CHANGELOG.md` (lines ~140, ~283) | "Fixed compilation speed regression in v0.48.0"; "Fixed compilation speed regression introduced in 0.42.0" |
| `Compile.hs:178` | optional `nested_ts_32` feature toggled into generated `Cargo.toml` |
| Wall-clock numbers | UNVERIFIED; no measured figures in the files read |
