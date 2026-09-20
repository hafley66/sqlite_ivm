# statement costs, 8_group_limit, 20 views x 8 shapes

`scripts/statement-costs.sh 8_group_limit <label>`; plain wall is `cargo test -q --test 8_group_limit`, three runs.

| step | commit | plain wall s | group_limit statements | fresh prepares | group_limit vm_step |
|---|---|---|---|---|---|
| baseline | 5f557a5 | 3.71 3.72 3.76 | 366219 | 265545 | 19481922 |
| prepare_cached in drain | see git log | 2.65 2.63 2.69 | 366219 | 17079 | 19481922 |

Instrumented run is 44s and 1.4M JSON lines; measurement only, never CI.
`nanos` has 1ms granularity on this SQLite; the ms column is a sample count.
