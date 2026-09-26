# Typed IR state layout for ivm-sqlite

Decision: option 2, delegated by the user to the coordinator on 2026-09-26.
Extend the existing `Compiled`, `Root`, `SettleSql`, and `frontier_catalog` path
to cover typed nodes and SCC rounds. `ivm-sqlite` ends with one persisted
program representation: typed IR. SQL text lowers to that IR through
`ivm-sql-frontend`, including on reattach. Keep the existing SQL program,
composition, collector, and reattach tests green during the transition.

`Engine::install(program, host: &mut impl Host)` is the lifecycle boundary.
`Raw` holds a caller-owned SQLite connection; `Plugin` uses the extension's
connection. The operator port is committed one group at a time. The lab
SQLite script and 1,000-seed random gates run against `ivm-sqlite` before the
lab directory is removed.

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

## Options considered

Which layout should the typed IR port use?

1. Add a typed-program catalog and persistent per-node tables in `ivm-sqlite`,
   alongside the existing SQL-text program path. Reattach loads the typed
   program and reconstructs its settle plan. The old path remains available
   for the extension and composition tests.
2. Extend the existing `Compiled`, `Root`, and `SettleSql` layout to represent
   typed nodes and SCC rounds. Persist typed IR in the existing catalog, and
   define how SQL-text programs map into that representation on reattach.

The second option is selected. The first option is retained here as the
alternative that was considered.
