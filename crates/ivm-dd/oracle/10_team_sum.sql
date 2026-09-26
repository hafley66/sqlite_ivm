-- Count and Sum only: the accumulable reduce path; a group empties, a sum hits zero.
CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW team_sum AS SELECT team, count(*), sum(cost) FROM job GROUP BY team;
-- step: f0
+ job 1 10 5
+ job 2 10 -5
+ job 3 20 4
-- step: f1_move_row
- job 3 20 4
+ job 3 10 4
-- step: f2_empty_group
- job 1 10 5
- job 2 10 -5
- job 3 10 4
