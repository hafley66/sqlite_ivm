-- S-N4: one relation on both join sides, bag output; p(2,2) joins itself.
CREATE TABLE p(x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY(x, y));
CREATE VIEW gp AS SELECT a.x, b.y FROM p AS a JOIN p AS b ON a.y = b.x;
-- step: f0_empty
-- step: f1_three_edges_one_batch
+ p 1 2
+ p 2 3
+ p 2 2
-- step: f2_remove_self_loop
- p 2 2
-- step: f3_close_cycle
+ p 3 1
-- step: f4_move_edge
- p 1 2
+ p 1 3
