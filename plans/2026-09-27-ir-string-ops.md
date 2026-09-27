# IR string operations

## Type signatures

```rust
// Cell remains i64; Ty::Id denotes dictionary identities, including strings.
Op::StrCons { input: NodeId, mode: StrMode }       // append Id (construct), or Id, Id (decompose)
StrMode::Construct { head: ColId, rest: ColId }
StrMode::Decompose { whole: ColId }
Func::StrNil() -> Cell                              // Ty::Id
```

Construct reads two string IDs, decodes their text, interns the concatenation, and appends its ID to each input row with the same weight. Decompose reads one string ID and appends IDs for its first Unicode scalar value and remaining text, or emits no row for the empty string. `StrNil` returns the empty string ID. The SQL and DD engines resolve IDs through the same logical dictionary; their numeric assignments need not match.

## IR choice

Use one `Op::StrCons` with two modes and scalar `Func::StrNil`. Construction can insert a dictionary entry, so it belongs beside `Mint`. Decomposition emits two columns and suppresses the empty case in one operation; separate scalar functions would also need a guard for the empty case. `TermLt` compares decoded string text before compound terms, matching `Universe::cmp`.

## Storage and uniqueness

SQLite extends `ivm_term_dict` with a nullable `text` column, `head_id` and `rest_id`, and a unique index on `text` for built-in string rows. Existing `(functor,args)` uniqueness remains for ordinary `Mint`; string rows use a reserved built-in functor and an empty argument list. The existing `id INTEGER PRIMARY KEY AUTOINCREMENT` allocates both kinds of term ID. DD's `Interner` adds `text -> id` and `id -> { text, head_id, rest_id }` maps alongside the existing constructor maps, using the same `next` counter. Inserting a nonempty string recursively interns its first character and remaining suffix, then stores those IDs. Dictionary entries never retract.

## Frontier reads and writes

At install, register the SQLite empty-ID scalar function and ensure the empty string dictionary row exists. DD interns empty text before dataflow starts. At each frontier, input literal IDs must resolve to text; construction reads head and rest, concatenates, inserts or gets the string ID and its split closure, then emits the weighted row. Decomposition reads `head_id` and `rest_id` and emits a row only for nonempty text. SQLite writes occur inside `ivm_engine_frontier`, so a failed frontier rolls back new strings. DD retains new entries across retractions, as it does for Mint. Results and test agreement compare decoded terms.

## Representation boundary requiring a decision

The current `Program`, `Frontier`, and `Host` APIs carry only `i64` cells. They have no text-bearing string literal or source-change representation. The fixture needs literal `"hi "` and source texts `"ada"` and `"hello"`; an engine cannot recover those bytes from an arbitrary `i64`. Before implementation, choose how text enters both engines, such as a program literal pool plus encoded references or a shared host interning API that supplies IDs for source changes. This also determines how generated oracle cases name strings without assuming equal numeric IDs across engines.
