import sqlite3, itertools, sys
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
Q="WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path"
def attempt(init, ops, q=Q):
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON; CREATE TABLE edge(a,b);")
    for r in init: c.execute("INSERT INTO edge VALUES(?,?)",r)
    c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{q}')")
    for op,r in ops:
        try:
            if op=="I": c.execute("INSERT INTO edge VALUES(?,?)",r)
            else: c.execute("DELETE FROM edge WHERE rowid IN (SELECT rowid FROM edge WHERE a IS ? AND b IS ? LIMIT 1)",r)
        except Exception as e: return "ERR "+str(e)
        from collections import Counter
        f=Counter(map(repr,c.execute(q))); i=Counter(map(repr,c.execute("select * from v")))
        if f!=i: return ("DIV",sorted((f-i).elements()),sorted((i-f).elements()))
    return None
# hand scenarios: real vs integer duplicates
print("A", attempt([(1,2),(2,3)], [("I",(2.0,3)),("D",(2,3))]))
print("B", attempt([(1,2),(2,3)], [("I",(2.0,3)),("D",(2.0,3))]))
print("C", attempt([(1,2.0)], [("I",(2,3)),("D",(1,2.0))]))
print("D", attempt([(1,2)], [("I",(1,2.0)),("D",(1,2))]))
print("E", attempt([(1,2)], [("I",(1,2.0)),("D",(1,2.0))]))
print("F", attempt([(1,2),(2,3)], [("I",(1,2.0)),("D",(1,2))]))
print("G", attempt([(1,2)], [("I",(2,3)),("I",(2.0,3)),("D",(2,3))]))
print("H", attempt([(1,2)], [("I",(2,3)),("I",(1,2.0)),("D",(1,2))]))
