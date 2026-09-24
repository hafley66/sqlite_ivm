-- S-N3: argmax row, max value with tie collapse (the Sprefa ROW_NUMBER() = 1 shape), top 2.
CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW top_job AS SELECT id, team, cost FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY team ORDER BY cost DESC, id ASC) AS rn FROM job) WHERE rn = 1;
CREATE VIEW max_cost AS SELECT DISTINCT team, cost FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY team ORDER BY cost DESC) AS rn FROM job) WHERE rn = 1;
CREATE VIEW top2 AS SELECT id, team, cost FROM
  (SELECT *, ROW_NUMBER() OVER (PARTITION BY team ORDER BY cost DESC, id ASC) AS rn FROM job) WHERE rn <= 2;
-- step: f0_tie_at_top
+ job 1 10 9
+ job 2 10 9
+ job 3 10 4
+ job 4 20 7
-- step: f1_winner_deleted_tie_survives
- job 1 10 9
-- step: f2_runner_up_takes_over
- job 2 10 9
-- step: d30_winner_moves_team
- job 4 20 7
+ job 4 10 7
-- step: f4_new_tie_resolved_by_id
+ job 5 10 7
