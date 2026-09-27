CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW numbered AS SELECT id, team, cost, ROW_NUMBER() OVER (PARTITION BY team ORDER BY cost DESC, id, team, cost) FROM job;
CREATE VIEW ranked AS SELECT id, team, cost, RANK() OVER (PARTITION BY team ORDER BY cost DESC) FROM job;
CREATE VIEW dense AS SELECT id, team, cost, DENSE_RANK() OVER (PARTITION BY team ORDER BY cost DESC) FROM job;
CREATE VIEW lagged AS SELECT id, team, cost, COALESCE(LAG(cost, 1, 0) OVER (PARTITION BY team ORDER BY cost DESC, id, team, cost), 0) FROM job;
CREATE VIEW led AS SELECT id, team, cost, COALESCE(LEAD(cost, 1, 0) OVER (PARTITION BY team ORDER BY cost DESC, id, team, cost), 0) FROM job;
CREATE VIEW running_sum AS SELECT id, team, cost, SUM(cost) OVER (PARTITION BY team ORDER BY cost DESC, id, team, cost ROWS UNBOUNDED PRECEDING) FROM job;
CREATE VIEW running_count AS SELECT id, team, cost, COUNT(*) OVER (PARTITION BY team ORDER BY cost DESC, id, team, cost ROWS UNBOUNDED PRECEDING) FROM job;
-- step: first_two_partitions
+ job 1 10 9
+ job 2 10 9
+ job 3 10 4
+ job 4 20 7
-- step: rerank_one_partition
- job 1 10 9
-- step: append_other_partition
+ job 5 20 8
-- step: move_partition
- job 3 10 4
+ job 3 20 4
-- step: empty_frontier
-- step: remove_partition
- job 2 10 9
