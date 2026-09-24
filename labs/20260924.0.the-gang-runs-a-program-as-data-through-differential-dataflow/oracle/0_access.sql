-- Steps copied from plans/engine-iso/2_oracle.sql; the view is the program under test.
CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));
CREATE VIEW access AS
SELECT person, resource FROM direct_grant
UNION
SELECT m.person, p.resource FROM membership AS m JOIN permission AS p ON p.team = m.team;
-- step: 0_initial
BEGIN;
INSERT INTO membership VALUES (1,10),(1,20);
INSERT INTO permission VALUES (10,100),(20,100);
INSERT INTO direct_grant VALUES (3,300);
COMMIT;
-- step: 1_both_join_inputs
BEGIN;
INSERT INTO membership VALUES (2,10);
INSERT INTO permission VALUES (10,200);
COMMIT;
-- step: 2_duplicate_union_support
INSERT INTO direct_grant VALUES (1,200);
-- step: 3_join_support_retract
DELETE FROM membership WHERE person=1 AND team=10;
-- step: 4_last_join_support
DELETE FROM permission WHERE team=20 AND resource=100;
-- step: 5_last_union_support
DELETE FROM direct_grant WHERE person=1 AND resource=200;
-- step: 6_savepoint_rollback
BEGIN;
SAVEPOINT discarded;
INSERT INTO direct_grant VALUES (4,400);
ROLLBACK TO discarded;
RELEASE discarded;
COMMIT;
-- step: 7_transaction_rollback
BEGIN;
INSERT INTO membership VALUES (5,10);
ROLLBACK;
-- step: 8_update
UPDATE permission SET resource=300 WHERE team=10 AND resource=200;
