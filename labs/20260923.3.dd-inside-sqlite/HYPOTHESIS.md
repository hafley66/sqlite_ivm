# DD inside SQLite: packet engine and extension

The `frontier-dd-packet` crate owns one DD graph for the access join under set
UNION and one for grouped `COUNT/SUM`. The benchmark's `dd` arm calls that
crate from Rust. The `frontier-dd-ext` cdylib uses the same crate from a SQLite
collector callback, and the benchmark's `dd-ext` arm loads that cdylib.

The extension accepts the packet's three-column integer source tables.
`SELECT dd_frontier_install('access')` watches `membership`, `permission`, and
`direct_grant`; `SELECT dd_frontier_install('team_cost')` watches `job`. These
names identify the packet cases rather than a general SQL compiler. The two
materialized tables are `dd_frontier_access` and `dd_frontier_team_cost`, with
corresponding signed `_delta` tables. `dd_frontier_drop(name)` removes a case.

For each source transaction, `sqlite_ext::watch` collects row images and calls
`on_batch` at `xSync`. The callback sends one signed batch to the persistent DD
worker, applies its net output changes to SQLite result tables, and increments
`dd_frontier_catalog.generation` in the same SQLite transaction. Negative
output changes are applied before positive changes, allowing a group to replace
its row under a unique `team` key.

If SQLite rolls back after DD advanced, its generation and result tables roll
back while the worker remains ahead. On the next source batch, the generation
mismatch causes a worker rebuild from the source rows visible in that batch.
The callback diffs that rebuilt snapshot against the rolled-back result table.
The integration test forces this mismatch and checks both snapshots and signed
output changes. A direct test of a separate virtual table failing *after* this
collector's `xSync` remains open.

The benchmark compares four placements using the same generated source trace
and recompute oracle. Its timings separate install, seed, batch, churn, and
snapshot read. SQLite arms also report allocator bytes and file-backed DB/WAL
bytes; hafley-observe PROFILE counts are available with `--observe=on`.
