# Access graph input

All columns are first-normal-form integers. `person`, `team`, and `resource` are IDs. The source tables are `membership(person, team)`, `permission(team, resource)`, and `direct_grant(person, resource)`.

```sql
SELECT person, resource FROM direct_grant
UNION
SELECT m.person, p.resource
FROM membership AS m
JOIN permission AS p ON p.team = m.team;
```

The output is a set. Internally, the join and union must retain enough support to retract only when the last derivation disappears. Each listed frontier is one atomic batch; all writes in a frontier settle before its output is observed.

| Frontier | Source changes | Required net output change |
| --- | --- | --- |
| `0_initial` | `+membership(1,10)`, `+membership(1,20)`, `+permission(10,100)`, `+permission(20,100)`, `+direct_grant(3,300)` | `+(1,100)`, `+(3,300)` |
| `1_both_join_inputs` | `+membership(2,10)`, `+permission(10,200)` | `+(1,200)`, `+(2,100)`, `+(2,200)` |
| `2_duplicate_union_support` | `+direct_grant(1,200)` | empty |
| `3_join_support_retract` | `-membership(1,10)` | empty |
| `4_last_join_support` | `-permission(20,100)` | `-(1,100)` |
| `5_last_union_support` | `-direct_grant(1,200)` | `-(1,200)` |
| `6_savepoint_rollback` | add `direct_grant(4,400)` inside a savepoint, then roll back to it | empty |
| `7_transaction_rollback` | add `membership(5,10)`, then roll back the transaction | empty |
| `8_update` | replace `permission(10,200)` with `permission(10,300)` | `-(2,200)`, `+(2,300)` |

At frontier 0, `(1,100)` has join support 2. Frontier 1 contains the join cross-term `membership(2,10) × permission(10,200)`; `(2,200)` must appear once. Frontier 3 reduces `(1,100)` support from 2 to 1 and `(1,200)` union support from 2 to 1, with no output change.

`2_oracle.sql` records the full support count after each committed frontier. `3_expected.tsv` is the expected output of that SQL. The public API's snapshot is the visible rows with support greater than zero, and `3a_deltas.tsv` records the difference between consecutive visible snapshots. The rolled-back frontiers have no new committed state.

The second query is in `3b_aggregate.sql`: `job(id,team,cost)` produces `team,COUNT(*),SUM(cost)`. It moves a job between teams in the same frontier that adds another job, changes a cost across zero, deletes the last job in a group, and rolls back an insertion. Its expected snapshots and signed deltas are in `3c_aggregate_expected.tsv` and `3d_aggregate_deltas.tsv`. Frontiers `4_empty` and `5_rollback` have zero snapshot rows, so they have no lines in the snapshot TSV; the delta TSV still names both. The shared trait must accept both cases without changing its method signatures.
