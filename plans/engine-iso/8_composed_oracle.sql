-- Independent SQLite oracle for dynamic union membership and a consumer join.
-- person 1 = Alice, person 2 = Bob. Every relation has integer 1NF cells.
-- Run from sqlite_ivm: sqlite3 -batch -bail -separator "$(printf '\t')" :memory: < plans/engine-iso/8_composed_oracle.sql | diff - plans/engine-iso/9_composed_expected.tsv
CREATE TABLE body_a(person INTEGER PRIMARY KEY);
CREATE TABLE body_b(person INTEGER PRIMARY KEY);
CREATE TABLE body_c(person INTEGER PRIMARY KEY);
CREATE TABLE grant_resource(person INTEGER PRIMARY KEY, resource INTEGER NOT NULL);
INSERT INTO body_a VALUES (1);
INSERT INTO body_b VALUES (1);
INSERT INTO body_c VALUES (1),(2);
INSERT INTO grant_resource VALUES (1,10),(2,20);

CREATE VIEW support AS
SELECT person, count(*) AS weight FROM (
  SELECT person FROM body_a UNION ALL SELECT person FROM body_b
) GROUP BY person;
CREATE VIEW visible AS SELECT person FROM support WHERE weight > 0;
CREATE VIEW consumer AS
SELECT v.person, g.resource FROM visible v
JOIN grant_resource g ON g.person = v.person;

CREATE TEMP TABLE previous(person INTEGER, resource INTEGER,
  PRIMARY KEY(person,resource));
CREATE TEMP VIEW changes AS
SELECT '+' AS sign, person, resource FROM (
  SELECT person,resource FROM consumer
  EXCEPT SELECT person,resource FROM previous
)
UNION ALL
SELECT '-' AS sign, person, resource FROM (
  SELECT person,resource FROM previous
  EXCEPT SELECT person,resource FROM consumer
);

SELECT '0_initial','support',person,weight FROM support ORDER BY person;
SELECT '0_initial',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '0_initial','delta_count',count(*),0 FROM changes;
INSERT INTO previous SELECT * FROM consumer;

-- Installing a third body adds a second support for Alice and first support
-- for Bob. Only Bob becomes visible downstream.
DROP VIEW support;
CREATE VIEW support AS
SELECT person, count(*) AS weight FROM (
  SELECT person FROM body_a UNION ALL SELECT person FROM body_b
  UNION ALL SELECT person FROM body_c
) GROUP BY person;
SELECT '1_add_body_c','support',person,weight FROM support ORDER BY person;
SELECT '1_add_body_c',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '1_add_body_c','delta_count',count(*),0 FROM changes;
DELETE FROM previous;
INSERT INTO previous SELECT * FROM consumer;

-- Removing one of Alice's supports leaves downstream visibility unchanged.
DROP VIEW support;
CREATE VIEW support AS
SELECT person, count(*) AS weight FROM (
  SELECT person FROM body_a UNION ALL SELECT person FROM body_c
) GROUP BY person;
SELECT '2_remove_body_b','support',person,weight FROM support ORDER BY person;
SELECT '2_remove_body_b',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '2_remove_body_b','delta_count',count(*),0 FROM changes;
DELETE FROM previous;
INSERT INTO previous SELECT * FROM consumer;

-- Removing C retracts Bob but keeps Alice through A.
DROP VIEW support;
CREATE VIEW support AS SELECT person, count(*) AS weight FROM body_a GROUP BY person;
SELECT '3_remove_body_c','support',person,weight FROM support ORDER BY person;
SELECT '3_remove_body_c',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '3_remove_body_c','delta_count',count(*),0 FROM changes;
DELETE FROM previous;
INSERT INTO previous SELECT * FROM consumer;

-- The union and the consumer's other join input change in one transaction.
BEGIN;
INSERT INTO body_a VALUES (2);
UPDATE grant_resource SET resource=21 WHERE person=2;
COMMIT;
SELECT '4_both_inputs','support',person,weight FROM support ORDER BY person;
SELECT '4_both_inputs',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '4_both_inputs','delta_count',count(*),0 FROM changes;
DELETE FROM previous;
INSERT INTO previous SELECT * FROM consumer;

-- A failed rule-set change leaves the previous graph and rows readable.
BEGIN;
DROP VIEW support;
CREATE VIEW support AS SELECT person, count(*) AS weight FROM body_c GROUP BY person;
ROLLBACK;
SELECT '5_rollback','support',person,weight FROM support ORDER BY person;
SELECT '5_rollback',sign,person,resource FROM changes ORDER BY sign,person,resource;
SELECT '5_rollback','delta_count',count(*),0 FROM changes;
