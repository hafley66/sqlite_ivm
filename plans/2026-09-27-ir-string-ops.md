# IR string operations

## Type signatures

```rust
// Cell remains i64; Ty::Id denotes dictionary identities, including strings.
Program.texts: Vec<String>
Expr::Text(u32) -> Ty::Id
Engine::intern_text(&mut self, text: &str, host: &mut impl Host) -> Result<Cell, EngineError>
Engine::text(&self, id: Cell, host: &mut impl Host) -> Result<Option<String>, EngineError>
Op::StrCons { input: NodeId, mode: StrMode }       // append Id (construct), or Id, Id (decompose)
StrMode::Construct { head: ColId, rest: ColId }
StrMode::Decompose { whole: ColId }
Func::StrNil() -> Cell                              // Ty::Id
```

Construct reads two string IDs, decodes their text, interns the concatenation, and appends its ID to each input row with the same weight. Decompose reads one string ID and appends IDs for its first Unicode scalar value and remaining text, or emits no row for the empty string. `StrNil` returns the empty string ID. The SQL and DD engines resolve IDs through the same logical dictionary; their numeric assignments need not match.

## IR choice

Use one `Op::StrCons` with two modes and scalar `Func::StrNil`. Construction can insert a dictionary entry, so it belongs beside `Mint`. Decomposition emits two columns and suppresses the empty case in one operation; separate scalar functions would also need a guard for the empty case. `TermLt` compares decoded string text before compound terms, matching `Universe::cmp`.

## Storage and uniqueness

Text is stored whole; minting a string mints only that string. SQLite stores each string as one `ivm_term` row under the reserved functor `@str`, one `ivm_text(id, text UNIQUE)` row, and one `ivm_term_sortkey` row. String and constructor terms share the `ivm_term` id space. DD's `Interner` keeps `text -> id` and `id -> Term { text }` maps alongside the constructor maps, using the same `next` counter. Neither dictionary stores a head/rest split or any per-character or per-suffix entry; a 1 MB text is one dictionary entry. Decompose splits on demand: it reads the whole text, takes the first Unicode scalar value and the remainder, and mints those two strings at that moment (DD: inside the `flat_map` that holds the interner; SQLite: fill statements of the decompose node, in the same frontier write that `Mint` and `Construct` use). Minting is a lookup on the text, then one insert; no mint path calls itself. Dictionary entries never retract.

## Frontier reads and writes

At install, register the SQLite empty-ID scalar function and ensure the empty string dictionary row exists. DD interns empty text before dataflow starts. At each frontier, input literal IDs must resolve to text; construction reads head and rest, concatenates, inserts or gets the whole string's ID, then emits the weighted row. Decomposition reads the whole text of each input row, inserts or gets the IDs of its first-character string and rest string, and emits a row only for nonempty text. SQLite writes occur inside `ivm_engine_frontier`, so a failed frontier rolls back new strings. DD retains new entries across retractions, as it does for Mint. Results and test agreement compare decoded terms.

## Text ingress

`Program.texts: Vec<String>` is an install-time literal pool; `Expr::Text(u32)` names an entry. Each engine interns the pool at install. `Engine::intern_text(&mut self, text: &str, host) -> Result<Cell>` supplies IDs for source changes, and `Engine::text(&self, id: Cell, host) -> Result<Option<String>>` decodes results. The engine-owned dictionary is the only string authority. Tests call `intern_text` on each engine before building its frontier and compare decoded terms, never numeric IDs across engines.
