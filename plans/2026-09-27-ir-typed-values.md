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

`Int` is the signed integer. `Real` is `f64::to_bits()` reinterpreted as `i64`. `Text` is the engine string dictionary ID. `Any` is `(class, payload)` stored through a tagged dictionary entry: class is SQLite's storage class, payload is the corresponding integer, real bits, or text ID. No NULL or BLOB support is added to the watched-source contract.

## Instance lifetimes and storage

One IR program persists in `frontier_catalog`. The SQLite engine stores node delta and integrated rows in `ivm_n*` tables, with integer cell payload columns. Its string dictionary persists in `ivm_term*` tables; tagged cells must remain stable across reopen. The DD engine stores rows in differential collections and text/tagged cells in its `Interner` for the engine lifetime. Source staging keeps native SQLite values, and `Get` converts each staged value before node evaluation. The public output view decodes values by the output relation types.

## Reads, writes, and uniqueness

At install, SQL-text parsing identifies source and output types, then writes the IR catalog row. Bootstrap reads native source rows into staging, interns text and tagged values, and writes typed cells into node tables. Settle repeats conversion for signed source changes. The string dictionary returns the same ID for the same string; tagged dictionary identity includes both class and payload. A numeric `5` and text `"5"` therefore have distinct keys. Output reads decode the cell by its relation type.

## Comparison and sums

`Int` compares signed. `Real` compares numeric `f64` values; integer and real cross-type comparisons compare numerically. `Text` compares decoded strings lexically. `Any` compares by SQLite class order: NULL < INTEGER/REAL < TEXT < BLOB; numeric classes compare numerically. `Id` retains structural term ordering. SQLite node SQL must compare decoded values rather than dictionary IDs for `Text` and `Any`; DD uses its interner. `SUM(Int)` remains integer and `SUM(Real)` accumulates `f64`, producing real bits. `COUNT` remains integer. Min, Max, TopK, and join equality use the same typed comparison rules.

## Sequence

1. Add IR type variants and expression/aggregate type propagation.
2. Implement source and output conversion, comparison, and sums in SQLite nodes and DD.
3. Lower SQL-text programs with source types and remove the legacy planner and its SQL builders.
4. Rewrite root-table assertions against snapshots and deltas, extend random generation, and run the requested gates.

## Blocked semantic

SQLite's storage-class order includes BLOB. An arbitrary BLOB cannot be held in an `i64` payload, `f64` bits, or the engine's UTF-8 string dictionary. A tagged `Any` cell still needs a blob payload ID and a persistent blob dictionary to decode and compare arbitrary bytes. That is a second payload encoding beyond the specified Text, Real, and tagged-cell representation. `encode_value` currently rejects BLOB. The stop condition applies before full SQLite class ordering can be implemented.

This checkpoint implements the bootstrap integer/text distinction and REAL sums, plus Text/Real comparison in node expressions, extrema, and TopK. Numeric equivalence between INTEGER and REAL in `Any` grouping or join keys and dynamic `SUM(Any)` remain unfinished. The existing watched-source NULL rejection remains in place.
