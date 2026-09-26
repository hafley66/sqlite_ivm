# Typed IR state layout for ivm-sqlite

The promotion has moved the IR, relational algebra, DD engine, and the existing
SQLite frontier engine into workspace crates. The SQLite operator port requires
one storage decision before code changes to `plan.rs` and `catalog.rs`.

The existing SQLite engine installs SQL text in `frontier_catalog`, recompiles
that SQL on reattach, and maintains one root per program. Its intermediate tables
are scan and join deltas; `Root` is either `Union` or `Group`.

The lab SQLite lowering takes a typed `ivm_ir::Program`. It assigns a delta and,
when needed, integrated table to each IR node. `Reduce` owns group and extrema
arrangements. `LetRec` owns SCC round, deletion, accumulator, and copy tables.
That engine currently opens an in-memory connection at install and has no
catalog or reattach path.

The two layouts cannot be combined by mapping `Op::Mfp`, `Op::Antijoin`,
`Op::TopK`, and `LetRec` to the current `Root` variants. `Engine::install` also
accepts only `&Program`, while the persistent engine needs the host's SQLite
connection. The host boundary and the persisted program representation must be
settled together.

## Boop-Ask

Which layout should the typed IR port use?

1. Add a typed-program catalog and persistent per-node tables in `ivm-sqlite`,
   alongside the existing SQL-text program path. Reattach loads the typed
   program and reconstructs its settle plan. The old path remains available
   for the extension and composition tests.
2. Extend the existing `Compiled`, `Root`, and `SettleSql` layout to represent
   typed nodes and SCC rounds. Persist typed IR in the existing catalog, and
   define how SQL-text programs map into that representation on reattach.

Both choices require a host-aware install entry point so `Plugin` can provide
the database connection and `Raw` can provide a caller-owned one.
