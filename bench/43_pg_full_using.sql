-- Observed with PostgreSQL 18.6 and pg_ivm 1.15.
-- The defining SELECT remains the correctness oracle.
CREATE EXTENSION IF NOT EXISTS pg_ivm;
CREATE SCHEMA full_using_repro;
SET search_path=full_using_repro,public;
CREATE TABLE a(id INTEGER PRIMARY KEY,k INTEGER);
CREATE TABLE b(id INTEGER PRIMARY KEY,k INTEGER);
SELECT pgivm.create_immv('result',
  'SELECT k,a.k AS ak,b.k AS bk,a.id AS aid,b.id AS bid FROM a FULL JOIN b USING(k)');
INSERT INTO a VALUES(1,1);
INSERT INTO b VALUES(1,1);
DELETE FROM b;
SELECT k,ak,bk,aid,bid FROM result;
-- Observed: NULL | 1 | NULL | 1 | NULL
SELECT k,a.k AS ak,b.k AS bk,a.id AS aid,b.id AS bid FROM a FULL JOIN b USING(k);
-- Expected:    1 | 1 | NULL | 1 | NULL
