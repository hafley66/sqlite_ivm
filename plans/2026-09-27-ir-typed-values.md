# Typed IR values and SQL-text lowering

## Type signatures

```rust
enum Ty { Int, Id, Text, Real, Any }
type Cell = i64;
fn lower_ir(plan: &Compiled, source_types: &dyn Fn(&str) -> Option<Vec<Ty>>) -> Result<Program, EngineError>;
fn compare_cells(a: Cell, a_ty: Ty, b: Cell, b_ty: Ty, text: impl Fn(Cell) -> Option<String>) -> Ordering;
fn sum_cell(acc: Cell, next: Cell, ty: Ty) -> Cell;
```

`Any` is needed for the untyped `mixed.k` source in the bootstrap test. It carries a storage-class tag and payload. Source schemas with a single declared storage class use `Int`, `Real`, or `Text`; no-affinity sources use `Any`. `Id` remains reserved for constructor terms. SQL-text output columns inherit source or aggregate types through `Program::node_types`.

## Cell encoding

`Int` is the signed integer. `Real` is `f64::to_bits()` reinterpreted as `i64`. `Text` is the engine string dictionary ID. `Any` is `(class, payload)` stored through a tagged dictionary entry: class is SQLite's storage class, payload is the corresponding integer, real bits, text ID, BLOB ID, or zero for NULL. BLOB bytes live in a separate persistent dictionary; `Any` cells carry its monotone ID.

## Instance lifetimes and storage

One IR program persists in `frontier_catalog`. The SQLite engine stores node delta and integrated rows in `ivm_n*` tables, with integer cell payload columns. Its string dictionary persists in `ivm_term*` tables, and BLOB bytes in `ivm_blob_dict`; tagged cells remain stable across reopen. The DD engine stores rows in differential collections and text/tagged/BLOB cells in its `Interner` for the engine lifetime. Source staging keeps native SQLite values, and `Get` converts each staged value before node evaluation. The public output view decodes values by the output relation types.

## Reads, writes, and uniqueness

At install, SQL-text parsing identifies source and output types, then writes the IR catalog row. Nullable source columns use `Any`. Bootstrap reads native source rows into staging, interns text, BLOB, and tagged values, and writes typed cells into node tables. Settle repeats conversion for signed source changes. The string and BLOB dictionaries return the same ID for the same payload; tagged dictionary identity includes class and payload. A numeric `5` and text `"5"` therefore have distinct keys. Output reads decode the cell by its relation type.

## Comparison and sums

`Int` compares signed. `Real` compares numeric `f64` values; integer and real cross-type comparisons compare numerically. `Text` compares decoded strings lexically. `Any` compares by SQLite class order: NULL < INTEGER/REAL < TEXT < BLOB; numeric classes compare numerically. `Id` retains structural term ordering. SQLite node SQL must compare decoded values rather than dictionary IDs for `Text` and `Any`; DD uses its interner. `SUM(Int)` remains integer and `SUM(Real)` accumulates `f64`, producing real bits. `COUNT` remains integer. Min, Max, TopK, and join equality use the same typed comparison rules.

## Sequence

1. Add IR type variants and expression/aggregate type propagation.
2. Implement source and output conversion, comparison, and sums in SQLite nodes and DD.
3. Lower SQL-text programs with source types and remove the legacy planner and its SQL builders.
4. Rewrite root-table assertions against snapshots and deltas, extend random generation, and run the requested gates.

## Value edge representation

The persistent BLOB payload dictionary supplies byte storage while tagged `Any` cells keep their `i64` IDs. Equality keys normalize exactly representable integral REAL values to their INTEGER counterpart; output rows retain the source storage class. Aggregate output values use the same tagged cell dictionary. Nullable watched columns use `Any`, so no additional cell encoding is required.

## SQLite value edge rules

1. `Any` numeric equality uses SQLite's INTEGER/REAL comparison. INTEGER `5` and REAL `5.0` share a group and join key, while their source storage classes remain available for output. An integral REAL outside the signed 64-bit range must not be rounded into an INTEGER key.
2. `SUM(Any)` skips NULL, converts numeric text and BLOB inputs by SQLite's numeric conversion, returns INTEGER while all non-NULL inputs are integers, and returns REAL when a REAL or non-integer input participates. An all-NULL group returns NULL. Integer-only overflow raises SQLite's integer-overflow error; `TOTAL` returns REAL and returns `0.0` for no non-NULL inputs.
3. Source NULL is a distinct `Any` class. NULL never satisfies join equality, including NULL against NULL. GROUP BY, DISTINCT, and set keys place NULLs together. COUNT(column), SUM, MIN, and MAX skip NULL; COUNT(*) counts the row.
4. BLOB values use a persistent byte payload dictionary in each engine, with monotone IDs and stable decoding across SQLite reopen. `Any` comparison orders BLOB after TEXT and compares BLOBs bytewise. BLOB equality is byte equality.
