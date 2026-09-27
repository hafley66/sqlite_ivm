-- program: 1_team_cost
-- S-W on the team-cost program; raw steps send exactly the listed frontier.
CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW team_cost AS
SELECT team, count(*), sum(cost), min(cost), max(cost) FROM job GROUP BY team;
CREATE VIEW job_count AS SELECT count(*) FROM job HAVING count(*) > 0;
-- step: base
+ job 2 10 7
-- step: d3_same_row_delete_reinsert
- job 2 10 7
+ job 2 10 7
-- step: d17_group_nets_to_zero_in_batch
+ job 5 30 4
- job 5 30 4
-- step: d18_group_sum_exactly_zero
+ job 6 40 5
+ job 7 40 -5
-- step: d19_setup
- job 7 40 -5
-- step: d19_sum_returns_to_zero
+ job 7 40 -5
-- step: d21_last_rows_deleted
- job 2 10 7
- job 6 40 5
- job 7 40 -5
