CREATE TABLE seed(n INTEGER PRIMARY KEY);
CREATE TABLE edge(x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY(x, y));
CREATE VIEW reachable AS WITH RECURSIVE r(n) AS (
  SELECT n FROM seed
  UNION
  SELECT e.y FROM r JOIN edge e ON e.x = r.n
) SELECT n FROM r;
-- step: seed_chain
+ seed 0
+ edge 0 1
+ edge 1 2
+ edge 2 3
-- step: add_cycle
+ edge 3 1
-- step: remove_feed
- edge 0 1
-- step: restore_feed
+ edge 0 1
-- step: remove_middle
- edge 1 2
-- step: restore_middle
+ edge 1 2
-- step: remove_seed
- seed 0
-- step: restore_seed
+ seed 0
