# statement costs, 8_group_limit, 20 views x 8 shapes

`scripts/statement-costs.sh 8_group_limit <label>`; plain wall is `cargo test -q --test 8_group_limit`, three runs.

| step | commit | plain wall s | statements inside drain | fresh prepares | group_limit vm_step |
|---|---|---|---|---|---|
| baseline | 5f557a5 | 3.71 3.72 3.76 | 631631 | 265545 | 19481922 |
| prepare_cached in drain | c99b003 | 2.65 2.63 2.69 | 631631 | 17079 | 19481922 |
| seed DELETEs, count(DISTINCT) x2, before DELETE dropped | ee28d17 | 2.55 2.43 2.45 | 514038 | 280 | 18443741 |
| refresh gated on the source DDL generation | 0292aa7 | 2.48 2.39 2.36 (overlaps the row above: no measurable change) | 514038 | 280 | 18443741 |

Instrumented run is 44s and 1.4M JSON lines; measurement only, never CI.
`nanos` has 1ms granularity on this SQLite; the ms column is a sample count.

## profile, 0292aa7, samply on the test binary

| where | share |
|---|---|
| `btreeBeginTrans -> getPageNormal -> memset/vm_allocate` (ephemeral b-tree open, Apple purgeable pcache) | 12% |
| `VdbeHalt -> BtreeClose -> purgeableCacheDestroy -> vm_deallocate` | 6% |
| `StatementCache::get` + `cache_stmt` (hash of long SQL, reset) | 20% |
| `alloc::fmt::format` + `columns()` + `out_table()` building SQL per call | 11% |
| `execute_batch` in the drain path | 16% |

| build, interleaved runs | wall s |
|---|---|
| system libsqlite3 | 2.94 2.72 2.89 |
| `--features bundled`, libsqlite3-sys opt-level 3 | 1.99 2.00 1.90 |
