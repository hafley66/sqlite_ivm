# IVM experiment preservation, 2026-09-09

The active native implementation is the standalone sqlite_ivm component.
Earlier work is retained by named Git snapshots before any removal from the
current tree. The inventory was presented for review before cleanup.

| Snapshot under ivm-history/2026-09-09/ | Commit | Contents |
|---|---|---|
| postgres-pglite | 2c4614e8d | Initial native PostgreSQL/PGlite comparison and receipts |
| postgres-crossover | 9b8ce8f41 | Shared crossover, trigger probes, SQL templates and Python full-refresh installer |
| sqlite-research | 46e918dac | Stock SQLite extension boundary research |
| sqlite-installer | 93ac8c74d | Earlier SQL installer, 20 circuit families, DD/SWI comparison and all remaining local circuit artifacts |
| sqlite-native-prototypes | 286200483 | C virtual-table prototype in 42_sqlite_native_take2 and C delta runtime/Rust SQL compiler in 43_sqlite_competitive |
| main-before-native | 40c6f5561 | Integrated earlier lab and its original main history |

The last installer preservation commit contains 5,487 files: two source/test
edits plus generated benchmark evidence. The 12k grid was incomplete; its empty
aggregate receipt is preserved and does not establish a pass. These snapshots
preserve observed state without rerunning or endorsing historical results.

The experiment lab path within those snapshots is
v6/labs/exec_shootout/postgres_pglite_ivm. The prototype subdirectories live
inside that lab. TASKS contains their briefs and reports.

To browse a preserved tree:

```bash
git ls-tree -r --name-only ivm-history/2026-09-09/sqlite-native-prototypes -- v6/labs/exec_shootout/postgres_pglite_ivm
# Extract into a separate directory without altering the current checkout:
mkdir -p /tmp/ivm-history-inspect
git archive ivm-history/2026-09-09/sqlite-native-prototypes v6/labs/exec_shootout/postgres_pglite_ivm | tar -x -C /tmp/ivm-history-inspect
```

Removal is a separate follow-up commit. Active benchmark dependencies must be
accounted for: v6/justfile, the sprefa-store crossover example, and PostgreSQL
reach benchmark adapters currently reference the earlier lab. The present
SQLite component has its own fixtures and competitor implementations in
bench/shared. Older sqlite_raw, sqlite_baseline, sprefa-store and DL7 runtimes
are outside this experiment cleanup inventory.
