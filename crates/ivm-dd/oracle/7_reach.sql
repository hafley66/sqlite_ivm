-- S-N2: recursion with negation on a relation outside the SCC; cycles, two paths, self-loop.
CREATE TABLE e(x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY(x, y));
CREATE TABLE blocked(n INTEGER PRIMARY KEY);
CREATE VIEW reach AS WITH RECURSIVE r(x, y) AS (
  SELECT e.x, e.y FROM e WHERE NOT EXISTS (SELECT 1 FROM blocked AS b WHERE b.n = e.y)
  UNION
  SELECT r.x, e.y FROM r JOIN e ON e.x = r.y WHERE NOT EXISTS (SELECT 1 FROM blocked AS b WHERE b.n = e.y)
) SELECT x, y FROM r;
-- step: f0_cycle_with_tail
+ e 1 2
+ e 2 3
+ e 3 1
+ e 3 4
-- step: f1_block_node_on_cycle
+ blocked 3
-- step: f2_unblock
- blocked 3
-- step: f3_break_cycle
- e 2 3
-- step: d22_two_paths_setup
+ e 10 11
+ e 11 13
+ e 10 12
+ e 12 13
-- step: d22_delete_one_path
- e 11 13
-- step: d23_two_cycle_terminates
+ e 20 21
+ e 21 20
-- step: d25_self_loop
+ e 30 30
-- step: d25_self_loop_delete
- e 30 30
-- step: d26_delete_readd_in_batch
- e 20 21
+ e 20 21
-- step: f9_rejoin_cycle
+ e 2 3
-- step: dred_cycle_fed_from_outside
+ e 41 42
+ e 42 41
+ e 43 41
-- step: dred_feed_removed_rows_self_support_only
- e 43 41
-- step: dred_feed_back_cycle_edge_out
+ e 43 41
- e 41 42
-- step: dred_cycle_edge_back_other_readded_in_batch
+ e 41 42
- e 42 41
+ e 42 41
