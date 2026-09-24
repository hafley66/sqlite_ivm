CREATE TABLE membership(person INTEGER NOT NULL, team INTEGER NOT NULL, PRIMARY KEY(person, team));
CREATE TABLE permission(team INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(team, resource));
CREATE TABLE direct_grant(person INTEGER NOT NULL, resource INTEGER NOT NULL, PRIMARY KEY(person, resource));

CREATE VIEW support AS
SELECT person, resource, count(*) AS weight
FROM (
  SELECT person, resource FROM direct_grant
  UNION ALL
  SELECT m.person, p.resource
  FROM membership AS m JOIN permission AS p ON p.team = m.team
)
GROUP BY person, resource;

BEGIN;
INSERT INTO membership VALUES (1,10),(1,20);
INSERT INTO permission VALUES (10,100),(20,100);
INSERT INTO direct_grant VALUES (3,300);
COMMIT;
SELECT '0_initial', person, resource, weight FROM support ORDER BY person, resource;

BEGIN;
INSERT INTO membership VALUES (2,10);
INSERT INTO permission VALUES (10,200);
COMMIT;
SELECT '1_both_join_inputs', person, resource, weight FROM support ORDER BY person, resource;

INSERT INTO direct_grant VALUES (1,200);
SELECT '2_duplicate_union_support', person, resource, weight FROM support ORDER BY person, resource;

DELETE FROM membership WHERE person=1 AND team=10;
SELECT '3_join_support_retract', person, resource, weight FROM support ORDER BY person, resource;

DELETE FROM permission WHERE team=20 AND resource=100;
SELECT '4_last_join_support', person, resource, weight FROM support ORDER BY person, resource;

DELETE FROM direct_grant WHERE person=1 AND resource=200;
SELECT '5_last_union_support', person, resource, weight FROM support ORDER BY person, resource;

BEGIN;
SAVEPOINT discarded;
INSERT INTO direct_grant VALUES (4,400);
ROLLBACK TO discarded;
RELEASE discarded;
COMMIT;
SELECT '6_savepoint_rollback', person, resource, weight FROM support ORDER BY person, resource;

BEGIN;
INSERT INTO membership VALUES (5,10);
ROLLBACK;
SELECT '7_transaction_rollback', person, resource, weight FROM support ORDER BY person, resource;

UPDATE permission SET resource=300 WHERE team=10 AND resource=200;
SELECT '8_update', person, resource, weight FROM support ORDER BY person, resource;
