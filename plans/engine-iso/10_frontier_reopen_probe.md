# Frontier collector restart probe, 2026-09-24

Canonical local main at `41369db`; Homebrew SQLite CLI and the release loadable
extension built by `bash scripts/0_build.sh release`. One file-backed database,
two SQLite processes. The source relation has one integer value column.

```bash
probe_dir=$(mktemp -d /private/tmp/frontier-reopen.XXXXXX)
probe_db="$probe_dir/reopen.sqlite"
probe_ext="$PWD/target/extension/release/libsqlite_ivm.dylib"
/opt/homebrew/opt/sqlite/bin/sqlite3 "$probe_db" "PRAGMA trusted_schema=ON; PRAGMA recursive_triggers=ON; SELECT load_extension('$probe_ext'); CREATE TABLE src(id INTEGER PRIMARY KEY, v INTEGER); SELECT sqlite_ivm_frontier_install('p','SELECT v FROM src'); INSERT INTO src VALUES (1,10); SELECT count(*) FROM frontier_p;"
/opt/homebrew/opt/sqlite/bin/sqlite3 "$probe_db" "PRAGMA trusted_schema=ON; PRAGMA recursive_triggers=ON; SELECT load_extension('$probe_ext'); INSERT INTO src VALUES (2,20); SELECT count(*) FROM frontier_p; SELECT frontier FROM frontier_catalog WHERE name='p';"
```

First process: install returns `p`, visible count is `1`. Second process:
`Parse error in 2nd command line argument: no such module: frontier_p_c1`.
The second INSERT does not complete.

The `sqlite_ext::watch` module registration is connection-local. The
`frontier_p_c1` virtual table persists in the schema, but loading the plugin
on a new connection does not register that named collector module. A restart
gate must install a reattach path for persisted collectors, then verify a new
connection can commit source writes and read the advanced frontier. Dropping
and reopening only the Rust `Program` handle on the original connection does
not exercise this gate.
