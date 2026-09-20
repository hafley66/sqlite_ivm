import sqlite3, random
from collections import Counter
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
def conn():
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;"); return c
Q={"fix":"WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p",
   "dist":"SELECT DISTINCT a FROM t",
   "grp":"SELECT a,count(*) AS n FROM t GROUP BY a",
   "join":"SELECT x.a AS a,y.b AS b FROM t x JOIN t y ON y.a=x.b",
   "left":"SELECT x.a AS a,y.b AS b FROM t x LEFT JOIN t y ON y.a=x.b",
   "cte2":"WITH base AS (SELECT a,b FROM t WHERE b IS NOT NULL), p(x,y) AS (SELECT a,b FROM base UNION SELECT p.x,e.b FROM p JOIN base e ON e.a=p.y) SELECT p.x AS x,p.y AS y FROM p JOIN base b ON b.a=p.x"}
def run(label,seed,n=40):
    rnd=random.Random(seed); c=conn(); c.execute("CREATE TABLE t(a TEXT COLLATE NOCASE,b TEXT COLLATE NOCASE)")
    rows=[]
    v=lambda: rnd.choice(['a','A','b','B','c','C','d'])
    for _ in range(rnd.randint(1,6)): r=(v(),v()); rows.append(r); c.execute("INSERT INTO t VALUES(?,?)",r)
    try: c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{Q[label]}')")
    except Exception as e: return ("UNSUPPORTED",str(e))
    hist=[]
    for _ in range(n):
        try:
            if rows and rnd.random()<0.5:
                r=rows.pop(rnd.randrange(len(rows))); hist.append(("D",r)); c.execute("DELETE FROM t WHERE rowid IN (SELECT rowid FROM t WHERE a=? COLLATE BINARY AND b=? COLLATE BINARY LIMIT 1)",r)
            else:
                r=(v(),v()); rows.append(r); hist.append(("I",r)); c.execute("INSERT INTO t VALUES(?,?)",r)
        except Exception as e: return (seed,hist,"ERR",str(e))
        f=Counter(map(repr,c.execute(Q[label]))); inc=Counter(map(repr,c.execute("select * from v")))
        if f!=inc:
            fo=sorted((f-inc).elements()); io=sorted((inc-f).elements())
            if sorted(x.lower() for x in fo)!=sorted(x.lower() for x in io): return (seed,hist,"DIV",fo,io)
    return None
for label in Q:
    bad=None
    for seed in range(200):
        r=run(label,seed)
        if r: bad=r; break
    print(label,"OK" if not bad else bad)
