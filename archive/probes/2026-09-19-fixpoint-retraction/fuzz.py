import sqlite3, random, sys
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
def conn():
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;")
    return c
QUERIES={
 "tc":  ("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path"),
 "tc2": ("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT e.a,p.y FROM edge e JOIN path p ON p.x=e.b) SELECT x,y FROM path"),
 "anti":("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT a,b FROM edge e WHERE NOT EXISTS(SELECT 1 FROM edge f WHERE f.a=e.b)"),
 "left":("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT e.a AS x,e.b AS y,f.b AS z FROM edge e LEFT JOIN edge f ON f.a=e.b"),
 "grp": ("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT a,count(*) AS n,sum(b) AS s FROM edge GROUP BY a HAVING count(*)>1"),
 "dist":("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT DISTINCT a FROM edge"),
 "lim": ("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT a,b FROM edge ORDER BY b,a LIMIT 3"),
 "full":("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT e.a AS x,e.b AS y,f.a AS z FROM edge e FULL JOIN edge f ON f.a=e.b"),
 "tcanti":("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path p WHERE NOT EXISTS(SELECT 1 FROM edge e WHERE e.a=p.x AND e.b=p.y)"),
 "tcgrp":("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,count(*) AS n FROM path GROUP BY x"),
 "tc2rules":("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT b,a FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path"),
 "tcpred":("CREATE TABLE edge(a INTEGER,b INTEGER)", "WITH RECURSIVE path(x,y,n) AS (SELECT a,b,1 FROM edge UNION SELECT p.x,e.b,p.n+1 FROM path p JOIN edge e ON e.a=p.y WHERE p.n<4) SELECT x,y,n FROM path"),
 "sub": ("CREATE TABLE edge(a INTEGER,b INTEGER)", "SELECT a FROM edge WHERE b IN (SELECT a FROM edge)"),
}
def run(name, seed, nverts=5, steps=40):
    ddl, q = QUERIES[name]
    rnd=random.Random(seed)
    c=conn(); c.execute(ddl)
    rows=[]
    for _ in range(rnd.randint(0,8)):
        r=(rnd.randint(1,nverts),rnd.randint(1,nverts)); rows.append(r); c.execute("INSERT INTO edge VALUES(?,?)",r)
    c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{q.replace(chr(39),chr(39)*2)}')")
    hist=[]
    for step in range(steps):
        if rows and rnd.random()<0.5:
            r=rows.pop(rnd.randrange(len(rows))); op=("DELETE",r)
            try: c.execute("DELETE FROM edge WHERE rowid IN (SELECT rowid FROM edge WHERE a=? AND b=? LIMIT 1)",r)
            except Exception as e: return (name,seed,hist+[op],"ERR",str(e))
        else:
            r=(rnd.randint(1,nverts),rnd.randint(1,nverts)); rows.append(r); op=("INSERT",r)
            try: c.execute("INSERT INTO edge VALUES(?,?)",r)
            except Exception as e: return (name,seed,hist+[op],"ERR",str(e))
        hist.append(op)
        fresh=sorted(c.execute(q).fetchall(),key=repr); inc=sorted(c.execute("SELECT * FROM v").fetchall(),key=repr)
        if fresh!=inc:
            return (name,seed,hist,"DIVERGE",fresh,inc)
    return None
names=sys.argv[1:] or list(QUERIES)
for name in names:
    bad=None
    for seed in range(300):
        r=run(name,seed)
        if r: bad=r; break
    print(name, "OK" if not bad else bad)
