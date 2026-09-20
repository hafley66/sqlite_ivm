.load /Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib
PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;
CREATE TABLE edge(a INTEGER, b INTEGER);
INSERT INTO edge VALUES(1,2),(2,3),(3,4),(1,3),(2,4);
CREATE VIRTUAL TABLE v USING sqlite_ivm('WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path');
.print --- before delete: view vs fresh
SELECT count(*) FROM v;
DELETE FROM edge WHERE a=2 AND b=3;
.print --- after delete (2,3): rows in fresh not in view
WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path EXCEPT SELECT x,y FROM v;
.print --- rows in view not in fresh
SELECT x,y FROM v EXCEPT WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path;
.print --- view
SELECT * FROM v ORDER BY 1,2;
