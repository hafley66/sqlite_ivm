import sqlite3, random, sys
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
def conn():
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;")
    return c
DDL="CREATE TABLE edge(a INTEGER,b INTEGER,t TEXT); CREATE TABLE node(id INTEGER, kind TEXT)"
TC="WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) "
QUERIES={
 "chain": "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y), path2(x,y) AS (SELECT x,y FROM path WHERE x=1 UNION SELECT q.x,p.y FROM path2 q JOIN path p ON p.x=q.y) SELECT x,y FROM path2",
 "tcjoin": TC+"SELECT p.x AS x,p.y AS y,n.kind AS k FROM path p JOIN node n ON n.id=p.y",
 "tcleft": TC+"SELECT p.x AS x,p.y AS y,n.kind AS k FROM path p LEFT JOIN node n ON n.id=p.y",
 "tcfilt": "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge WHERE t='r' UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y WHERE e.t<>'x') SELECT x,y FROM path",
 "tcnode": "WITH RECURSIVE reach(id) AS (SELECT id FROM node WHERE kind='root' UNION SELECT e.b FROM reach r JOIN edge e ON e.a=r.id) SELECT id FROM reach",
 "tcnode2": "WITH RECURSIVE reach(id) AS (SELECT id FROM node WHERE kind='root' UNION SELECT e.b FROM reach r JOIN edge e ON e.a=r.id JOIN node n ON n.id=e.b) SELECT id FROM reach",
 "tctext": "WITH RECURSIVE path(x,y,t) AS (SELECT a,b,t FROM edge UNION SELECT p.x,e.b,e.t FROM path p JOIN edge e ON e.a=p.y) SELECT x,y,t FROM path",
 "tcdist": TC+"SELECT DISTINCT x FROM path",
 "tcunion": TC+"SELECT x AS v FROM path UNION SELECT y FROM path",
 "tcexcept": TC+"SELECT x AS v,y AS w FROM path EXCEPT SELECT a,b FROM edge",
 "tcgrpjoin": TC+"SELECT x,count(*) AS n FROM path p JOIN node n ON n.id=p.y GROUP BY x HAVING count(*)>1",
 "tcsemi": TC+"SELECT a,b FROM edge e WHERE EXISTS(SELECT 1 FROM path p WHERE p.y=e.a)",
 "tcanti2": TC+"SELECT id FROM node n WHERE NOT EXISTS(SELECT 1 FROM path p WHERE p.y=n.id)",
 "mutual": "WITH RECURSIVE ev(x) AS (SELECT a FROM edge WHERE a=1 UNION SELECT e.b FROM ev JOIN edge e ON e.a=ev.x) SELECT x FROM ev",
 "selfagg": TC+"SELECT x, (SELECT count(*) FROM path q WHERE q.x=p.x) AS n FROM path p",
 "tcrow": TC+"SELECT x,y,row_number() OVER (PARTITION BY x ORDER BY y) AS rn FROM path",
}
def run(name, seed, nverts=5, steps=40):
    q = QUERIES[name]
    rnd=random.Random(seed)
    c=conn(); c.executescript(DDL)
    rows=[]; nodes=[]
    for i in range(1,nverts+1): c.execute("INSERT INTO node VALUES(?,?)",(i,rnd.choice(['root','leaf','mid'])))
    for _ in range(rnd.randint(0,8)):
        r=(rnd.randint(1,nverts),rnd.randint(1,nverts),rnd.choice(['r','x','y'])); rows.append(r); c.execute("INSERT INTO edge VALUES(?,?,?)",r)
    try: c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{q.replace(chr(39),chr(39)*2)}')")
    except Exception as e: return (name,"UNSUPPORTED",str(e))
    hist=[]
    for step in range(steps):
        p=rnd.random()
        try:
            if rows and p<0.4:
                r=rows.pop(rnd.randrange(len(rows))); op=("DELETE",r)
                c.execute("DELETE FROM edge WHERE rowid IN (SELECT rowid FROM edge WHERE a=? AND b=? AND t=? LIMIT 1)",r)
            elif p<0.55:
                i=rnd.randint(1,nverts); k=rnd.choice(['root','leaf','mid']); op=("UPDNODE",i,k)
                c.execute("UPDATE node SET kind=? WHERE id=?",(k,i))
            elif rows and p<0.7:
                r=rows.pop(rnd.randrange(len(rows))); r2=(rnd.randint(1,nverts),rnd.randint(1,nverts),r[2]); rows.append(r2); op=("UPDEDGE",r,r2)
                c.execute("UPDATE edge SET a=?,b=? WHERE rowid IN (SELECT rowid FROM edge WHERE a=? AND b=? AND t=? LIMIT 1)",r2[:2]+r)
            else:
                r=(rnd.randint(1,nverts),rnd.randint(1,nverts),rnd.choice(['r','x','y'])); rows.append(r); op=("INSERT",r)
                c.execute("INSERT INTO edge VALUES(?,?,?)",r)
        except Exception as e: return (name,seed,hist+[op],"ERR",str(e))
        hist.append(op)
        fresh=sorted(c.execute(q).fetchall(),key=repr); inc=sorted(c.execute("SELECT * FROM v").fetchall(),key=repr)
        if fresh!=inc:
            return (name,seed,hist,"DIVERGE",sorted(set(fresh)-set(inc)),sorted(set(inc)-set(fresh)),c.execute("select * from edge").fetchall(),c.execute("select * from node").fetchall())
    return None
names=sys.argv[1:] or list(QUERIES)
for name in names:
    bad=None
    for seed in range(150):
        r=run(name,seed)
        if r: bad=r; break
    print(name, "OK" if not bad else bad)
