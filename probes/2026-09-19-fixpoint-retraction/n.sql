.load /Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib
PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
CREATE TABLE t(a TEXT COLLATE NOCASE, b TEXT COLLATE NOCASE);
CREATE VIRTUAL TABLE v USING sqlite_ivm('WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p');
INSERT INTO t VALUES('B','b');
INSERT INTO t VALUES('A','c');
INSERT INTO t VALUES('a','b');
.print --- view after inserts
SELECT x,y FROM v ORDER BY 1,2;
.print --- member table all (op table with __k UNIQUE)
SELECT name FROM sqlite_master WHERE name LIKE 'v_op%';
SELECT __k,c0,c1 FROM v_op2_2;
DELETE FROM t WHERE a='a' COLLATE BINARY AND b='b' COLLATE BINARY;
.print --- view after delete of (a,b)
SELECT x,y FROM v ORDER BY 1,2;
INSERT INTO t VALUES('c','d');
.print --- delete (A,c): expected error
DELETE FROM t WHERE a='A' COLLATE BINARY AND b='c' COLLATE BINARY;
.print --- fresh
WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p ORDER BY 1,2;
.print --- view
SELECT x,y FROM v ORDER BY 1,2;
