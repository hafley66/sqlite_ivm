.load /Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib
PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
CREATE TABLE t(a TEXT COLLATE NOCASE, b TEXT COLLATE NOCASE);
INSERT INTO t VALUES('a','b'),('c','B');
CREATE VIRTUAL TABLE v USING sqlite_ivm('WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p');
INSERT INTO t VALUES('A','c');
DELETE FROM t WHERE a='a' COLLATE BINARY AND b='b' COLLATE BINARY;
.print --- member table v_op5_2 (__k, c0, c1) and state before the failing delete
SELECT __k,c0,c1 FROM v_op5_2;
SELECT __key,c0,c1 FROM v_state;
DELETE FROM t WHERE a='A' COLLATE BINARY AND b='c' COLLATE BINARY;
.print --- view / fresh after
SELECT x,y FROM v ORDER BY 1,2;
WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p ORDER BY 1,2;
