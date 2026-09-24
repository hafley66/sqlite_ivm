# 1. Substrait as the operator-IR contract

Retrieved 2026-09-24. Spec source: `substrait-io/substrait` main @ `d149822` (2026-09-24), latest release **v0.103.1** (2026-09-20). Spec is pre-1.0; breaking changes land with `!` in commit titles (e.g. #1136 `refactor(extensions)!`).

Primary sources:
- Proto: [algebra.proto](https://github.com/substrait-io/substrait/blob/main/proto/substrait/algebra.proto), [plan.proto](https://github.com/substrait-io/substrait/blob/main/proto/substrait/plan.proto), [extensions.proto](https://github.com/substrait-io/substrait/blob/main/proto/substrait/extensions/extensions.proto), [type.proto](https://github.com/substrait-io/substrait/blob/main/proto/substrait/type.proto)
- Docs: [logical relations](https://substrait.io/relations/logical_relations/), [physical relations](https://substrait.io/relations/physical_relations/), [extensions](https://substrait.io/extensions/), [type classes](https://substrait.io/types/type_classes/), [binary serialization](https://substrait.io/serialization/binary_serialization/), [text serialization](https://substrait.io/serialization/text_serialization/), [serialization basics](https://substrait.io/serialization/basics/)
- Schemas: [simple_extensions_schema.yaml](https://github.com/substrait-io/substrait/blob/main/text/simple_extensions_schema.yaml), [dialect_schema.yaml](https://github.com/substrait-io/substrait/blob/main/text/dialect_schema.yaml)

---

## 1. Relation types

`message Rel { oneof rel_type { ... } }` in algebra.proto, verbatim field numbers:

| oneof field | # | message | class |
|---|---|---|---|
| `read` | 1 | ReadRel | logical |
| `filter` | 2 | FilterRel | logical |
| `fetch` | 3 | FetchRel | logical |
| `aggregate` | 4 | AggregateRel | logical |
| `sort` | 5 | SortRel | logical |
| `join` | 6 | JoinRel | logical |
| `lateral_join` | 24 | LateralJoinRel | logical |
| `project` | 7 | ProjectRel | logical |
| `set` | 8 | SetRel | logical |
| `extension_single` | 9 | ExtensionSingleRel | extension |
| `extension_multi` | 10 | ExtensionMultiRel | extension |
| `extension_leaf` | 11 | ExtensionLeafRel | extension |
| `cross` | 12 | CrossRel | logical |
| `reference` | 21 | ReferenceRel | logical (DAG/sharing) |
| `write` | 19 | WriteRel | logical (DML) |
| `ddl` | 20 | DdlRel | logical (DDL) |
| `update` | 22 | UpdateRel | logical (DML) |
| `hash_join` | 13 | HashJoinRel | physical |
| `merge_join` | 14 | MergeJoinRel | physical |
| `nested_loop_join` | 18 | NestedLoopJoinRel | physical |
| `window` | 17 | ConsistentPartitionWindowRel | physical (listed under "Physical relations" comment) |
| `exchange` | 15 | ExchangeRel | physical |
| `expand` | 16 | ExpandRel | physical |
| `top_n` | 23 | TopNRel | physical |

Every rel carries `RelCommon common = 1` (except ReferenceRel) and `AdvancedExtension advanced_extension = 10` (except Extension*Rel and ReferenceRel).

### 1.1 RelCommon (shared header)

| field | type | semantics |
|---|---|---|
| `emit_kind` oneof: `direct` / `emit` | `Direct {}` / `Emit { repeated int32 output_mapping }` | output column selection/reorder applied after the rel's own output |
| `hint` | `Hint { Stats stats; RuntimeConstraint constraint; string alias; repeated string output_names; saved_computations; loaded_computations }` | non-semantic; `SavedComputation`/`LoadedComputation` share hashtables / bloom filters across rels by `computation_id` |
| `advanced_extension` | AdvancedExtension | see section 3 |
| `rel_anchor` | `optional uint32` (>=1) | plan-unique id; binding point for `OuterReference.rel_reference` (lateral join, correlated subquery) |

### 1.2 Logical relations

| rel | fields (proto) | semantics |
|---|---|---|
| [ReadRel](https://substrait.io/relations/logical_relations/#read-operator) | `NamedStruct base_schema`; `Expression filter`; `Expression best_effort_filter`; `MaskExpression projection`; oneof `read_type`: `virtual_table` (`repeated Expression.Nested.Struct expressions`), `local_files`, `named_table` (`repeated string names`), `extension_table` (`google.protobuf.Any detail`), `iceberg_table` | leaf scan; `filter` mandatory, `best_effort_filter` may be partially applied |
| [FilterRel](https://substrait.io/relations/logical_relations/#filter-operator) | `Rel input`; `Expression condition` | keep rows where condition is true |
| [ProjectRel](https://substrait.io/relations/logical_relations/#project-operator) | `Rel input`; `repeated Expression expressions` | output = input columns ++ expressions (use `emit` to drop input columns) |
| [CrossRel](https://substrait.io/relations/logical_relations/#cross-product-operator) | `Rel left`; `Rel right` | Cartesian product |
| [JoinRel](https://substrait.io/relations/logical_relations/#join-operator) | `Rel left`; `Rel right`; `Expression expression`; `Expression post_join_filter`; `JoinType type` | see join-type table below |
| [LateralJoinRel](https://substrait.io/relations/logical_relations/#lateral-join-operator) | same as JoinRel; must set `common.rel_anchor` | right evaluated once per left row, may reference left via `OuterReference.rel_reference`; valid types: INNER, LEFT, LEFT_SEMI, LEFT_ANTI, LEFT_SINGLE, LEFT_MARK |
| [SetRel](https://substrait.io/relations/logical_relations/#set-operation) | `repeated Rel inputs` (>=2; first = primary); `SetOp op` | see set-op table below |
| [FetchRel](https://substrait.io/relations/logical_relations/#fetch-operator) | `Rel input`; `Expression offset_expr` (null/unset = 0); `Expression count_expr` (null/unset = ALL) | LIMIT/OFFSET |
| [SortRel](https://substrait.io/relations/logical_relations/#sort-operator) | `Rel input`; `repeated SortField sorts` | ORDER BY |
| [AggregateRel](https://substrait.io/relations/logical_relations/#aggregate-operator) | `Rel input`; `repeated Grouping groupings` (`repeated uint32 expression_references`); `repeated Measure measures` (`AggregateFunction measure; Expression filter`); `repeated Expression grouping_expressions` | GROUP BY / GROUPING SETS; >1 grouping set appends an `i32` set-ordinal column |
| [ReferenceRel](https://substrait.io/relations/logical_relations/#reference-operator) | `int32 subtree_ordinal` | points at `Plan.relations[subtree_ordinal]`; the sharing mechanism for DAGs (and, per maintainers, CTEs; see section 3.4) |
| [WriteRel](https://substrait.io/relations/logical_relations/#write-operator) | `table_schema`; `WriteOp op` (INSERT/DELETE/UPDATE/CTAS); `Rel input`; `CreateMode create_mode`; `OutputMode output` (NO_OUTPUT / MODIFIED_RECORDS); oneof `write_type` (`named_table` / `extension_table`) | sink / DML |
| [UpdateRel](https://substrait.io/relations/logical_relations/#update-operator) | `named_table`; `table_schema`; `Expression condition`; `repeated TransformExpression transformations` (`transformation`, `column_target`) | UPDATE ... SET ... WHERE |
| [DdlRel](https://substrait.io/relations/logical_relations/#ddl-operator) | `DdlObject object` (TABLE/VIEW); `DdlOp op` (CREATE, CREATE_OR_REPLACE, ALTER, DROP, DROP_IF_EXIST); `table_schema`; `table_defaults`; `Rel view_definition` | DDL |

JoinRel.JoinType enum:

| value | # | output |
|---|---|---|
| `JOIN_TYPE_INNER` | 1 | matched pairs |
| `JOIN_TYPE_OUTER` | 2 | full outer |
| `JOIN_TYPE_LEFT` / `_RIGHT` | 3 / 4 | left/right outer |
| `JOIN_TYPE_LEFT_SEMI` / `_RIGHT_SEMI` | 5 / 8 | rows of one side having >=1 match; only that side's columns |
| `JOIN_TYPE_LEFT_ANTI` / `_RIGHT_ANTI` | 6 / 9 | rows of one side having 0 matches (Datalog negation shape) |
| `JOIN_TYPE_LEFT_SINGLE` / `_RIGHT_SINGLE` | 7 / 10 | at most one match per row, error otherwise (scalar subquery) |
| `JOIN_TYPE_LEFT_MARK` / `_RIGHT_MARK` | 11 / 12 | all rows of one side + nullable boolean mark column |

SetRel.SetOp enum:

| value | # | SQL |
|---|---|---|
| `SET_OP_MINUS_PRIMARY` | 1 | EXCEPT DISTINCT |
| `SET_OP_MINUS_PRIMARY_ALL` | 7 | EXCEPT ALL |
| `SET_OP_MINUS_MULTISET` | 2 | primary minus rows present in all secondaries |
| `SET_OP_INTERSECTION_PRIMARY` | 3 | primary rows present in any secondary, deduped |
| `SET_OP_INTERSECTION_MULTISET` | 4 | INTERSECT DISTINCT |
| `SET_OP_INTERSECTION_MULTISET_ALL` | 8 | INTERSECT ALL (min multiplicity) |
| `SET_OP_UNION_DISTINCT` | 5 | UNION |
| `SET_OP_UNION_ALL` | 6 | UNION ALL |

### 1.3 Physical relations ([page](https://substrait.io/relations/physical_relations/))

| rel | fields | semantics |
|---|---|---|
| HashJoinRel | `left`, `right`, `repeated ComparisonJoinKey keys`, `post_join_filter`, own `JoinType` enum, `BuildInput build_input` | hash build/probe |
| MergeJoinRel | `left`, `right`, `keys`, `post_join_filter`, own `JoinType` | inputs pre-sorted on keys |
| NestedLoopJoinRel | `left`, `right`, `expression`, own `JoinType` | predicate over cross product |
| ConsistentPartitionWindowRel | `Rel input`; `repeated WindowRelFunction window_functions` (`function_reference`, `arguments`, `options`, `output_type`, `phase`, `invocation`, `lower_bound`, `upper_bound`, `bounds_type`); `repeated Expression partition_expressions`; `repeated SortField sorts` | all window functions share one partition + order; appends one column per function |
| ExchangeRel | `Rel input`; `int32 partition_count`; `repeated ExchangeTarget targets`; oneof kind: `scatter_by_fields`, `single_target`, `multi_target`, `round_robin`, `broadcast` | redistribution |
| ExpandRel | `Rel input`; `repeated ExpandField fields` (`SwitchingField` or consistent expression) | row duplication (GROUPING SETS lowering) |
| TopNRel | `input`, `sorts`, `Expression offset = 4`, `Expression count = 5`, `FetchMode mode = 6` (WITH_TIES option) | sort + fetch |

Docs-only (no proto; see issue [#870](https://github.com/substrait-io/substrait/issues/870), [#458](https://github.com/substrait-io/substrait/issues/458)): Merging Capture, Simple Capture, Hash Aggregate, Streaming Aggregate, Hashing Window, Streaming Window.

Window functions also exist as expressions: `Expression.WindowFunction` (algebra.proto) usable inside ProjectRel.

Subqueries: `Expression.Subquery` oneof `scalar` / `in_predicate` / `set_predicate` (EXISTS/UNIQUE) / `set_comparison` (ANY/ALL).

### 1.4 Recursive / CTE

- No `RecursiveRel`, `CteRel`, `FixpointRel`, `IterateRel`, or `UnionRecursive` message exists in algebra.proto (grep of main @ `d149822`: zero hits for `recurs`, `fixpoint`, `iterate` as rel names).
- `PlanRel.rel` comment: `// Any relation (used for references and CTEs)`.
- [serialization/basics](https://substrait.io/serialization/basics/): "a graph of nodes (typically a DAG unless the query is recursive)".
- [Logical relations, Reference Operator](https://substrait.io/relations/logical_relations/#reference-operator) text describes DAG construction.
- Issue [#613](https://github.com/substrait-io/substrait/issues/613) (open): maintainer reply: "There is a class of essentially recursive plans that we don't have an explicit definition for at the moment but probably should. These are Common Table Expressions (CTEs). As such I don't think we want to outlaw recursion ... At the moment it is up to the consumer to reject any plan it finds uncomfortably recursive." Resolution in thread: no restriction on `subtree_ordinal`; cycles (self-reference) permitted in principle; semantics of iteration undefined.
- DataFusion producer: `LogicalPlan::RecursiveQuery(plan) => not_impl_err!("Unsupported plan type: ...")` ([producer/rel/mod.rs](https://github.com/apache/datafusion/blob/main/datafusion/substrait/src/logical_plan/producer/rel/mod.rs)).
- Issue [#1217](https://github.com/substrait-io/substrait/issues/1217) / PR [#1218](https://github.com/substrait-io/substrait/pull/1218) (open) "recursive" refers to protobuf nesting depth (detached expression subtrees via `PlanRel.detached_expressions`), unrelated to recursive queries.

---

## 2. Type system and function extensions

### 2.1 Types ([type_classes](https://substrait.io/types/type_classes/), type.proto)

| category | members |
|---|---|
| simple | `boolean`, `i8`, `i16`, `i32`, `i64`, `fp32`, `fp64`, `string`, `binary`, `date`, `interval_year`, `uuid`, `unbound` (placeholder for partially bound plans) |
| compound | `FIXEDCHAR<L>`, `VARCHAR<L>`, `FIXEDBINARY<L>`, `DECIMAL<P,S>` (P<=38), `STRUCT<T1..Tn>`, `NSTRUCT<N:T..>` (pseudo-type), `LIST<T>`, `MAP<K,V>`, `FUNC<(T..)->R>` (lambda), `PRECISION_TIME<P>`, `PRECISION_TIMESTAMP<P>`, `PRECISION_TIMESTAMP_TZ<P>`, `INTERVAL_DAY<P>`, `INTERVAL_COMPOUND<P>` |
| user-defined | declared in extension YAML `types:`; referenced as `u!name`; in plan via `type_reference` -> `ExtensionType.type_anchor` |
| type variations | `type_variation_reference` on every type message; declared in YAML `type_variations:`; anchor 0 = system-preferred variation |
| nullability | every type carries `Nullability nullability` (`NULLABILITY_UNSPECIFIED/NULLABLE/REQUIRED`) |
| schema | `NamedStruct { repeated string names (DFS order); Type.Struct struct }`; core type system is ordinal, names are metadata |
| aliases | `Plan.type_aliases` (plan-level type aliasing, [type_aliases.md](https://github.com/substrait-io/substrait/blob/main/site/docs/types/type_aliases.md)) |

### 2.2 Simple extensions (YAML)

Top-level YAML keys per [simple_extensions_schema.yaml](https://github.com/substrait-io/substrait/blob/main/text/simple_extensions_schema.yaml): `urn`, `dependencies`, `metadata`, `types`, `type_variations`, `scalar_functions`, `aggregate_functions`, `window_functions`.

Per-impl keys (functions): `args` (`value` / `enum` / `type` args), `options`, `variadic`, `sessionDependent`, `deterministic`, `nullability` (MIRROR / DECLARED_OUTPUT / DISCRETE), `return`, `implementation`; aggregate-only: `intermediate`, `decomposable` (NONE/ONE/MANY), `maxset`, `ordered`; window-only: `window_type`.

Example (verbatim head of `extensions/functions_aggregate_generic.yaml`):

```yaml
urn: extension:io.substrait:functions_aggregate_generic
aggregate_functions:
  - name: "count"
    impls:
      - args:
          - name: x
            value: any
        options:
          overflow:
            values: [SILENT, SATURATE, ERROR]
        nullability: DECLARED_OUTPUT
        decomposable: MANY
        intermediate: i64
        return: i64
```

Standard YAML files in `extensions/`: `functions_aggregate_approx`, `functions_aggregate_decimal_output`, `functions_aggregate_generic`, `functions_arithmetic`, `functions_arithmetic_decimal`, `functions_boolean`, `functions_comparison`, `functions_datetime`, `functions_geometry`, `functions_list`, `functions_logarithmic`, `functions_rounding`, `functions_rounding_decimal`, `functions_set`, `functions_string`, `unsigned_integers` (16 files).

### 2.3 Identification: URN + anchors

The spec moved from URIs to URNs (`extension_uri_anchor` -> `extension_urn_anchor`, PR #1028 and related; exact migration release UNVERIFIED). Format: `extension:<OWNER>:<ID>`, OWNER in reverse-DNS (`io.substrait`, `com.example`). A draft IANA URN namespace registration was added in PR #1119 (2026-09-14).

extensions.proto signatures:

```proto
message SimpleExtensionURN {
  uint32 extension_urn_anchor = 1;
  string urn = 2;                       // extension:<OWNER>:<ID>
}
message SimpleExtensionDeclaration {
  oneof mapping_type {
    ExtensionType extension_type = 1;               // {extension_urn_reference=4, type_anchor=2, name=3}
    ExtensionTypeVariation extension_type_variation = 2; // {extension_urn_reference=4, type_variation_anchor=2, name=3}
    ExtensionFunction extension_function = 3;       // {extension_urn_reference=4, function_anchor=2, name=3}
  }
}
message AdvancedExtension {
  repeated google.protobuf.Any optimization = 1;   // ignorable
  google.protobuf.Any enhancement = 2;             // semantic, cannot be ignored
}
```

Plan-level wiring (plan.proto):

```proto
message Plan {
  Version version = 6;                                           // required after 0.17.0
  repeated substrait.extensions.SimpleExtensionURN extension_urns = 8;
  repeated substrait.extensions.SimpleExtensionDeclaration extensions = 2;
  repeated PlanRel relations = 3;                                // PlanRel { oneof { Rel rel = 1; RelRoot root = 2; } }
  substrait.extensions.AdvancedExtension advanced_extensions = 4;
  repeated string expected_type_urls = 5;                        // Any type URLs used in the plan
  repeated DynamicParameterBinding parameter_bindings = 7;
  // type_aliases (see plan.proto)
}
```

- Anchors are plan-local surrogate keys; `*_anchor` defines, `*_reference` uses (`function_reference` in `ScalarFunction`, `AggregateFunction`, `WindowFunction`).
- Function name in a declaration is a compound signature: `<name>:<short_arg_type0>_<short_arg_type1>...`, e.g. `equal:any_any`, `add:i64_i64`.

### 2.4 Dialects

[dialect_schema.yaml](https://github.com/substrait-io/substrait/blob/main/text/dialect_schema.yaml): a consumer-side capability declaration. Keys: `name`, `metadata`, `dependencies` (alias -> URN), `supported_types`, `supported_relations` (enum list incl. `READ`...`TOP_N`, or per-rel objects), plus functions. Extension rels are declared with `relation: EXTENSION_SINGLE | EXTENSION_MULTI | EXTENSION_LEAF` and `message_types: [<Any type URL>, ...]`. This is the machine-readable form of "runtime X implements rel set Y + Any types Z".

---

## 3. Custom relations: Delay, Fixpoint, signed-delta

### 3.1 Available hooks

| hook | location | carries | consumer obligation |
|---|---|---|---|
| `ExtensionLeafRel { RelCommon common = 1; google.protobuf.Any detail = 2; }` | Rel oneof 11 | 0 inputs | "Producers and consumers must agree on how to derive the output schema" |
| `ExtensionSingleRel { RelCommon common = 1; Rel input = 2; google.protobuf.Any detail = 3; }` | Rel oneof 9 | 1 input | same |
| `ExtensionMultiRel { RelCommon common = 1; repeated Rel inputs = 2; google.protobuf.Any detail = 3; }` | Rel oneof 10 | N inputs | same |
| `AdvancedExtension.enhancement` | every standard rel (field 10), `RelCommon`, `Plan` | 1 Any | must understand or reject |
| `AdvancedExtension.optimization` | same | repeated Any | may ignore |
| `ReadRel.ExtensionTable.detail` / `WriteRel` `extension_table` | leaf/sink | Any | custom source/sink |
| `ReferenceRel.subtree_ordinal` | Rel oneof 21 | index | shared subplans; cycles permitted per #613 thread |
| `Plan.expected_type_urls` | Plan | list of Any URLs | early reject of unknown detail types |
| user-defined types / type variations | YAML | `u!name`, variation anchors | may pass through values unchanged |
| user-defined aggregate functions | YAML | `intermediate`, `decomposable` | standard AggregateRel measures |

Output schema of Extension*Rel is not derivable by generic tooling; `RelRoot.names` comment: "For extension relations, the output type is determined by the extension relation contract."

### 3.2 Precedent: Arrow Acero

Arrow defines its own Any payloads in [cpp/proto/substrait/extension_rels.proto](https://github.com/apache/arrow/blob/main/cpp/proto/substrait/extension_rels.proto), `package arrow.substrait_ext`:

```proto
message AsOfJoinRel { repeated AsOfJoinKey keys = 1; int64 tolerance = 2; }
message NamedTapRel { string kind = 1; string name = 2; repeated string columns = 3; }
message SegmentedAggregateRel {
  repeated substrait.Expression.FieldReference grouping_keys = 1;
  repeated substrait.Expression.FieldReference segment_keys = 2;
  repeated substrait.AggregateRel.Measure measures = 3;
}
```

Consumed in `relation_internal.cc` via `case kExtensionLeaf / kExtensionSingle / kExtensionMulti`. Pattern: Any payloads may embed standard Substrait messages (`Expression.FieldReference`, `AggregateRel.Measure`).

### 3.3 Mapping IVM operators onto the hooks

Shapes below use only the hooks from 3.1. Message names are hypothetical (`dl8.ivm.*`).

| IVM op | carrier | Any payload (sketch) | output schema contract |
|---|---|---|---|
| Delay z^-1 | `ExtensionSingleRel` | `message Delay { uint32 lag = 1; }` | = input schema |
| Integrate / Differentiate (DBSP I, D) | `ExtensionSingleRel` | `message Integrate {}`, `message Differentiate {}` | = input schema |
| Fixpoint (recursive stratum) | `ExtensionMultiRel` with inputs = [seed/base rels ...], body referencing a loop variable | `message Fixpoint { uint32 loop_var_id = 1; Rel body = 2; FixpointMode mode = 3; /* SET vs BAG, max_iters */ }` + `ExtensionLeafRel{ LoopVar { uint32 id } }` inside `body` | = loop var schema |
| Fixpoint (alternative) | `ReferenceRel` cycle: `Plan.relations[k]` = `SetRel(UNION_*)` whose input contains `ReferenceRel{subtree_ordinal:k}` | iteration semantics in `AdvancedExtension.enhancement` on the SetRel | = SetRel schema; cycle semantics undefined by spec (#613) |
| Signed delta / Z-set weight, column form | explicit `i64` weight column in every schema + `AdvancedExtension.enhancement` (e.g. `message ZSetSemantics { uint32 weight_field = 1; }`) on each standard rel, or at `Plan.advanced_extensions` | weight column is ordinary data; enhancement declares that JoinRel multiplies, AggregateRel sums, SetRel UNION_ALL adds, MINUS negates | standard rels' schema + weight column |
| Signed delta, rel form | wrap standard rels: `ExtensionSingleRel{Delta}` over source reads; downstream rels unchanged | `message DeltaSource { string table = 1; }` in `ReadRel.ExtensionTable` | table schema (+ weight) |
| Distinct on Z-set (clamp weight to 1) | `ExtensionSingleRel` | `message Distinct {}` | = input |
| Arrangement / index hint | `AdvancedExtension.optimization` on JoinRel, or `RelCommon.hint.saved_computations` (`COMPUTATION_TYPE_HASHTABLE`) | `message ArrangeBy { repeated FieldReference keys = 1; }` | n/a (ignorable) |

Relevant existing fields:
- `AggregateFunction.phase` (`AggregationPhase`: `INITIAL_TO_INTERMEDIATE`, `INTERMEDIATE_TO_INTERMEDIATE`, `INITIAL_TO_RESULT`, `INTERMEDIATE_TO_RESULT`) and YAML `intermediate` / `decomposable` describe partial-aggregate state; no retraction/inverse operation is specified for aggregates.
- SetRel `*_ALL` ops define bag (multiplicity) semantics; there is no negative multiplicity in any op definition.

### 3.4 Streaming / incremental / delta / recursion in Substrait today

| concept | present in spec? | evidence |
|---|---|---|
| streaming rel (unbounded input, watermark, emit-on-change) | no | proto grep: zero hits for `watermark`, `changelog`, `retract`, `delta`, `incremental`, `stream` as rel/field names; "Streaming Aggregate/Window" are docs-only physical ops over sorted input |
| Z-set / signed multiplicity | no | SetRel ops defined over non-negative multiplicities |
| delay / time / epoch / frontier | no | no such fields |
| recursive query / CTE | undefined; cycles via ReferenceRel tolerated | #613 thread; `PlanRel.rel` comment "used for references and CTEs"; serialization basics "DAG unless the query is recursive" |
| GitHub issue search (substrait-io/substrait) for `streaming`, `incremental`, `changelog`, `retraction`, `watermark`, `fixpoint` | 0 issues about streaming/IVM semantics | hits: #458 (hash/streaming aggregate proto), #870 (docs-only physical ops), #1217 (proto nesting depth) |
| GitHub discussions | #125 "Recursive and union types" (data types, not queries); #809 "What features do we need to reach 1.0" (no streaming/recursion item in first 40 comments) | |
| Streaming engines publishing Substrait IVM extensions (Materialize, Feldera, RisingWave, Arroyo) | none found | web search 2026-09-24; UNVERIFIED as exhaustive |

---

## 4. Implementations

### 4.1 Rust

| crate | version (2026-09-24) | license | contents |
|---|---|---|---|
| [`substrait`](https://crates.io/crates/substrait) ([repo](https://github.com/substrait-io/substrait-rs)) | 0.65.0 (2026-08-27), edition 2024, MSRV 1.88 | Apache-2.0 | re-exports generated types from `substrait-prost`; features `extensions` (serde_yaml YAML types), `parse` (validated plan parsing: hex, thiserror, semver), `serde`, `protoc`, `protox`, `embed-descriptor`, `semver`; deps `prost 0.14.1`, `serde_json`, `indexmap` |
| [`substrait-prost`](https://crates.io/crates/substrait-prost) ([repo substrait-packaging](https://github.com/substrait-io/substrait-packaging)) | 0.103.1 (substrait 0.65.0 pins `=0.102.0`) | Apache-2.0 | prost-generated `substrait::proto::*`; `serde` feature = `pbjson` 0.9 + `pbjson-types` (proto3-JSON serde impls); `reflect` = `prost-reflect` 0.16 |
| [`substrait-extensions`](https://crates.io/crates/substrait-extensions) | 0.103.1 | Apache-2.0 | packaged standard YAML extensions |
| [`datafusion-substrait`](https://crates.io/crates/datafusion-substrait) | 55.1.0 (2026-09-11) | Apache-2.0 | producer + consumer for DataFusion `LogicalPlan`; deps `substrait ^0.63.0`, `prost ^0.14.1`, `pbjson-types ^0.8.0` |

Rust generated type path: `substrait::proto::{Plan, Rel, rel::RelType, JoinRel, join_rel::JoinType, ExtensionSingleRel, ...}`; `detail: Option<pbjson_types::Any>` / `prost_types::Any` depending on `serde` feature (UNVERIFIED which Any type is used under each feature).

datafusion-substrait extension surface ([substrait_consumer.rs](https://github.com/apache/datafusion/blob/main/datafusion/substrait/src/logical_plan/consumer/substrait_consumer.rs)):

```rust
#[async_trait]
pub trait SubstraitConsumer: Send + Sync + Sized {
    async fn consume_rel(&self, rel: &Rel) -> Result<LogicalPlan>;
    async fn consume_read(&self, rel: &ReadRel) -> Result<LogicalPlan>;
    async fn consume_filter(&self, rel: &FilterRel) -> Result<LogicalPlan>;
    async fn consume_join(&self, rel: &JoinRel) -> Result<LogicalPlan>;
    // ... fetch, aggregate, sort, project, set, cross, exchange
    async fn consume_consistent_partition_window(&self, rel) -> Result<LogicalPlan>; // default: not_impl
    async fn consume_extension_leaf(&self, rel: &ExtensionLeafRel) -> Result<LogicalPlan>;   // default: error "Missing handler"
    async fn consume_extension_single(&self, rel: &ExtensionSingleRel) -> Result<LogicalPlan>;
    async fn consume_extension_multi(&self, rel: &ExtensionMultiRel) -> Result<LogicalPlan>;
    // + consume_expression / consume_scalar_function / consume_subquery / consume_lambda ...
}
pub struct DefaultSubstraitConsumer<'a> { /* wraps SessionState */ }
```

`DefaultSubstraitConsumer` routes Extension*Rel via `SerializerRegistry` (datafusion-expr `registry.rs`):

```rust
pub trait SerializerRegistry: Debug + Send + Sync {
    fn serialize_logical_plan(&self, node: &dyn UserDefinedLogicalNode) -> Result<Vec<u8>>;
    fn deserialize_logical_plan(&self, name: &str, bytes: &[u8]) -> Result<Arc<dyn UserDefinedLogicalNode>>;
}
```

Producer (`producer/rel/mod.rs`) `not_impl` list: `AsOfJoin`, `Subquery`, `Statement`, `Explain`, `Analyze`, `Dml`, `Ddl`, `Copy`, `DescribeTable`, `Unnest`, `RecursiveQuery`. `LogicalPlan::Extension` -> `handle_extension` (emits Extension*Rel via SerializerRegistry). Consumer `ConsistentPartitionWindowRel`, `SwitchExpression`, `MultiOrList`, `Enum` -> `not_impl`.

### 4.2 Other producers / consumers

| project | role | status (2026-09-24) | link |
|---|---|---|---|
| substrait-java: `core`, `isthmus` (Calcite SQL <-> Substrait), `isthmus-cli`, `spark` | producer + consumer | release v0.103.0 (2026-09-06), active | [repo](https://github.com/substrait-io/substrait-java) |
| substrait-python | builders, YAML, validation | active (push 2026-09-23) | [repo](https://github.com/substrait-io/substrait-python) |
| substrait-go | Go types + plan builder | active (push 2026-09-23) | [repo](https://github.com/substrait-io/substrait-go) |
| substrait-cpp | C++ lib + text plan tooling | last push 2026-04-10 | [repo](https://github.com/substrait-io/substrait-cpp) |
| substrait-validator | Rust validator (plan -> diagnostics) | active (push 2026-09-24) | [repo](https://github.com/substrait-io/substrait-validator) |
| DuckDB `substrait` extension (`get_substrait`, `get_substrait_json`, `from_substrait`, `from_substrait_json`) | producer + consumer | now under `substrait-io/duckdb-substrait-extension`, release v1.5.5.1 (2026-08-25); distributed as community extension (UNVERIFIED) | [repo](https://github.com/substrait-io/duckdb-substrait-extension) |
| Apache Arrow Acero (`arrow/engine/substrait`, `pyarrow.substrait.run_query`) | consumer (+ limited producer for expressions) | in tree; handles read, filter, project, join, fetch, sort, aggregate, set, Extension* (AsOfJoin, NamedTap, SegmentedAggregate) | [dir](https://github.com/apache/arrow/tree/main/cpp/src/arrow/engine/substrait) |
| Velox | consumer | substrait converter **deleted** from Velox: commit `4bedd5ce` "refactor: Delete substrait (#13938)" 2025-06-30 | [velox](https://github.com/facebookincubator/velox) |
| Apache Gluten (Spark -> Velox/ClickHouse) | producer (Spark) + consumer (`cpp/velox/substrait/SubstraitToVelox*`) | carries the Velox converter | [dir](https://github.com/apache/incubator-gluten/tree/main/cpp/velox/substrait) |
| ibis-substrait | producer (Ibis expr -> Substrait) | last release v4.0.1 (2024-07-29); repo pushed 2026-09-18 | [repo](https://github.com/ibis-project/ibis-substrait) |
| DataFusion (Rust), incl. datafusion-python | producer + consumer | see 4.1 | [crate](https://crates.io/crates/datafusion-substrait) |

---

## 5. Serialization

| form | spec status | notes |
|---|---|---|
| binary protobuf | normative ([binary_serialization](https://substrait.io/serialization/binary_serialization/)) | top-level `substrait.Plan` or `substrait.ExtendedExpression`; proto package `substrait`, Java `io.substrait.proto`, C# `Substrait.Protobuf` |
| JSON | "The recommended text serialization format is JSON" ([text_serialization](https://substrait.io/serialization/text_serialization/)); in practice = proto3 canonical JSON mapping (lowerCamelCase keys, enum names as strings) as emitted by pbjson (Rust), JsonFormat (Java), `get_substrait_json` (DuckDB) | page also mentions an OpenAPI 3.1 schema + ANTLR expression grammar; the OpenAPI schema is not present in repo (UNVERIFIED as never published) |
| text plan formats | non-normative per-library (substrait-cpp textplan, substrait-java/isthmus string form) | ANTLR grammars in [`grammar/`](https://github.com/substrait-io/substrait/tree/main/grammar) are `SubstraitType.g4` (type strings) and `FuncTestCase*.g4` (function test files) |
| protobuf nesting limit | default recursion limit 100 in C++/Java parsers | #1217 / PR #1218 (open) add `PlanRel.detached_expressions` to bound depth |

### 5.1 Minimal join plan: measured size

Query: `SELECT * FROM a JOIN b ON a.x = b.y`, `a(x i64 NOT NULL)`, `b(y i64 NOT NULL)`, one extension URN, one function declaration (`equal:any_any`), version 0.103.1.

Text-proto input (encoded with `protoc --encode=substrait.Plan` against substrait main @ `d149822`):

```
version { minor_number: 103 patch_number: 1 }
extension_urns { extension_urn_anchor: 1 urn: "extension:io.substrait:functions_comparison" }
extensions { extension_function { extension_urn_reference: 1 function_anchor: 1 name: "equal:any_any" } }
relations { root {
  names: "x" names: "y"
  input { join {
    type: JOIN_TYPE_INNER
    left  { read { base_schema { names: "x" struct { types { i64 { nullability: NULLABILITY_REQUIRED } } nullability: NULLABILITY_REQUIRED } } named_table { names: "a" } } }
    right { read { base_schema { names: "y" struct { types { i64 { nullability: NULLABILITY_REQUIRED } } nullability: NULLABILITY_REQUIRED } } named_table { names: "b" } } }
    expression { scalar_function {
      function_reference: 1
      output_type { bool { nullability: NULLABILITY_REQUIRED } }
      arguments { value { selection { direct_reference { struct_field { field: 0 } } root_reference {} } } }
      arguments { value { selection { direct_reference { struct_field { field: 1 } } root_reference {} } } }
    } }
  } }
} }
```

| encoding | bytes | method |
|---|---|---|
| protobuf binary | **180** | `protoc --encode` (measured) |
| protobuf binary, gzip -9 | 179 | measured |
| proto3 JSON, minified | **1003** | hand-written per proto3 JSON mapping, minified with `json.dumps(separators=(',',':'))`; not produced by a Substrait library (UNVERIFIED byte-exact vs pbjson output, which may differ in default-value omission) |
| text-proto, whitespace stripped | 837 | measured |

Of the 180 binary bytes: the URN string is 43 bytes, `equal:any_any` 13 bytes.
