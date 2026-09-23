CREATE TABLE job(id INTEGER PRIMARY KEY, team INTEGER NOT NULL, cost INTEGER NOT NULL);
CREATE VIEW team_cost AS
SELECT team, count(*) AS jobs, sum(cost) AS total_cost
FROM job GROUP BY team;

BEGIN;
INSERT INTO job VALUES (1,10,5),(2,10,7),(3,20,11);
COMMIT;
SELECT '0_initial', team, jobs, total_cost FROM team_cost ORDER BY team;

BEGIN;
INSERT INTO job VALUES (4,10,3);
UPDATE job SET team=10 WHERE id=3;
COMMIT;
SELECT '1_move_and_add', team, jobs, total_cost FROM team_cost ORDER BY team;

UPDATE job SET cost=-7 WHERE id=2;
SELECT '2_cross_zero', team, jobs, total_cost FROM team_cost ORDER BY team;

BEGIN;
DELETE FROM job WHERE id IN (1,4);
COMMIT;
SELECT '3_delete_two', team, jobs, total_cost FROM team_cost ORDER BY team;

DELETE FROM job WHERE id IN (2,3);
SELECT '4_empty', team, jobs, total_cost FROM team_cost ORDER BY team;

BEGIN;
INSERT INTO job VALUES (5,30,9);
ROLLBACK;
SELECT '5_rollback', team, jobs, total_cost FROM team_cost ORDER BY team;
