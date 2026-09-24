-- Steps copied from plans/engine-iso/3b_aggregate.sql, program widened with min, max and a global count.
CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW team_cost AS
SELECT team, count(*), sum(cost), min(cost), max(cost) FROM job GROUP BY team;
CREATE VIEW job_count AS SELECT count(*) FROM job HAVING count(*) > 0;
-- step: 0_initial
BEGIN;
INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11);
COMMIT;
-- step: 1_move_and_add
BEGIN;
INSERT INTO job VALUES (4,10,3);
UPDATE job SET team=10 WHERE id=3;
COMMIT;
-- step: 2_cross_zero
UPDATE job SET cost=-7 WHERE id=2;
-- step: 3_delete_two
BEGIN;
DELETE FROM job WHERE id IN (1,4);
COMMIT;
-- step: 4_empty
DELETE FROM job WHERE id IN (2,3);
-- step: 5_rollback
BEGIN;
INSERT INTO job VALUES (5,30,9);
ROLLBACK;
