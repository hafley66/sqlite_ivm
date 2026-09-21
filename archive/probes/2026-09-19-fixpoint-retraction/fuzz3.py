import sqlite3, random, sys
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
def conn():
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;")
    return c
DDL="CREATE TABLE edge(a,b,t); CREATE TABLE node(id, kind)"
TC="WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) "
QUERIES={
 "tc": TC+"SELECT x,y FROM path",
 "tcnull": "WITH RECURSIVE path(x,y,t) AS (SELECT a,b,t FROM edge UNION SELECT p.x,e.b,CASE WHEN e.t=p.t THEN e.t END FROM path p JOIN edge e ON e.a=p.y) SELECT x,y,t FROM path",
 "tcdistin": "WITH RECURSIVE path(x,y) AS (SELECT DISTINCT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN (SELECT DISTINCT a,b FROM edge) e ON e.a=p.y) SELECT x,y FROM path",
 "tcgrpin": "WITH RECURSIVE path(x,y) AS (SELECT a,min(b) FROM edge GROUP BY a UNION SELECT p.x,e.b FROM path p JOIN (SELECT a,max(b) AS b FROM edge GROUP BY a) e ON e.a=p.y) SELECT x,y FROM path",
 "tcall": "WITH RECURSIVE path(x,y,n) AS (SELECT a,b,1 FROM edge UNION ALL SELECT p.x,e.b,p.n+1 FROM path p JOIN edge e ON e.a=p.y WHERE p.n<3) SELECT x,y,n FROM path",
 "tcnodeleft": "WITH RECURSIVE reach(id,k) AS (SELECT id,kind FROM node WHERE kind='root' UNION SELECT e.b,n.kind FROM reach r JOIN edge e ON e.a=r.id LEFT JOIN node n ON n.id=e.b) SELECT id,k FROM reach",
 "tcnodeanti": "WITH RECURSIVE reach(id) AS (SELECT id FROM node WHERE kind='root' UNION SELECT e.b FROM reach r JOIN edge e ON e.a=r.id WHERE NOT EXISTS(SELECT 1 FROM node n WHERE n.id=e.b AND n.kind='leaf')) SELECT id FROM reach",
 "selfjoin": TC+"SELECT p.x AS x,q.y AS y FROM path p JOIN path q ON q.x=p.y",
 "tcsum": TC+"SELECT x,sum(y) AS s,count(*) AS n FROM path GROUP BY x",
 "tcglobal": TC+"SELECT count(*) AS n FROM path",
 "twoin": "WITH RECURSIVE reach(id) AS (SELECT id FROM node WHERE kind='root' UNION SELECT e.b FROM reach r JOIN edge e ON e.a=r.id JOIN node n ON n.id=e.b AND n.kind<>'leaf') SELECT id FROM reach",
 "anchorjoin": "WITH RECURSIVE reach(id) AS (SELECT e.b FROM edge e JOIN node n ON n.id=e.a WHERE n.kind='root' UNION SELECT e.b FROM reach r JOIN edge e ON e.a=r.id) SELECT id FROM reach",
 "twosteps": "WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y UNION SELECT e.a,p.y FROM path p JOIN edge e ON e.b=p.x) SELECT x,y FROM path",
 "concat": "WITH RECURSIVE path(x,y,s) AS (SELECT a,b,a||''/''||b FROM edge UNION SELECT p.x,e.b,p.s||''/''||e.b FROM path p JOIN edge e ON e.a=p.y WHERE length(p.s)<12) SELECT x,y,s FROM path",
 "ineq": "WITH RECURSIVE path(x,y,w) AS (SELECT a,b,t FROM edge UNION SELECT p.x,e.b,e.t FROM path p JOIN edge e ON e.a=p.y AND e.t>=p.w) SELECT x,y,w FROM path",
 "subjoin": "WITH RECURSIVE reach(id) AS (SELECT id FROM node WHERE kind='root' UNION SELECT e.b FROM reach r JOIN (SELECT e.a,e.b FROM edge e JOIN node n ON n.id=e.a WHERE n.kind<>'leaf') e ON e.a=r.id) SELECT id FROM reach",
 "cte2": "WITH base AS (SELECT a,b FROM edge WHERE t IS NOT NULL), path(x,y) AS (SELECT a,b FROM base UNION SELECT p.x,e.b FROM path p JOIN base e ON e.a=p.y) SELECT p.x AS x,p.y AS y FROM path p JOIN base b ON b.a=p.x",
 "outer_after": TC+"SELECT n.id AS id, p.y AS y FROM node n LEFT JOIN path p ON p.x=n.id WHERE n.kind<>'leaf'",
 "anti_after": TC+"SELECT n.id AS id FROM node n WHERE NOT EXISTS(SELECT 1 FROM path p WHERE p.x=n.id AND p.y=n.id)",
 "two": "WITH RECURSIVE up(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM up p JOIN edge e ON e.a=p.y), down(x,y) AS (SELECT b,a FROM edge UNION SELECT p.x,e.a FROM down p JOIN edge e ON e.b=p.y) SELECT u.x AS x,u.y AS y FROM up u JOIN down d ON d.x=u.x AND d.y=u.y",
}
import os
MODE=os.environ.get("MODE","int")
def val(rnd,nv):
    r=rnd.random()
    if MODE=="int": return rnd.randint(1,nv)
    if MODE=="intnull": return None if r<0.1 else rnd.randint(1,nv)
    if MODE=="text": return "n%d"%rnd.randint(1,nv)
    if r<0.1: return None
    if r<0.2: return float(rnd.randint(1,nv))
    if r<0.3: return str(rnd.randint(1,nv))
    return rnd.randint(1,nv)
def run(name, seed, nverts=int(os.environ.get("NV","6")), steps=int(os.environ.get("STEPS","50"))):
    q = QUERIES[name]
    rnd=random.Random(seed)
    c=conn(); c.executescript(DDL)
    rows=[]
    for i in range(1,nverts+1): c.execute("INSERT INTO node VALUES(?,?)",(val(rnd,nverts),rnd.choice(['root','leaf','mid'])))
    for _ in range(rnd.randint(0,10)):
        r=(val(rnd,nverts),val(rnd,nverts),rnd.choice(['r','x',None])); rows.append(r); c.execute("INSERT INTO edge VALUES(?,?,?)",r)
    try: c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{q.replace(chr(39),chr(39)*2)}')")
    except Exception as e: return (name,"UNSUPPORTED",str(e))
    hist=[]
    for step in range(steps):
        p=rnd.random()
        try:
            if rows and p<0.35:
                r=rows.pop(rnd.randrange(len(rows))); op=("DELETE",r)
                c.execute("DELETE FROM edge WHERE rowid IN (SELECT rowid FROM edge WHERE a IS ? AND b IS ? AND t IS ? LIMIT 1)",r)
            elif rows and p<0.45:
                a=rows[0][0]; op=("DELETEALL",a); rows=[r for r in rows if r[0]!=a or (r[0] is None) != (a is None)]
                c.execute("DELETE FROM edge WHERE a IS ?",(a,))
                rows=[tuple(x) for x in c.execute("SELECT a,b,t FROM edge").fetchall()]
            elif p<0.55:
                i=val(rnd,nverts); k=rnd.choice(['root','leaf','mid']); op=("UPDNODE",i,k)
                c.execute("UPDATE node SET kind=? WHERE id IS ?",(k,i))
            elif p<0.62:
                i=val(rnd,nverts); k=rnd.choice(['root','leaf','mid']); op=("INSNODE",i,k)
                c.execute("INSERT INTO node VALUES(?,?)",(i,k))
            elif p<0.68:
                i=val(rnd,nverts); op=("DELNODE",i)
                c.execute("DELETE FROM node WHERE rowid IN (SELECT rowid FROM node WHERE id IS ? LIMIT 1)",(i,))
            else:
                r=(val(rnd,nverts),val(rnd,nverts),rnd.choice(['r','x',None])); rows.append(r); op=("INSERT",r)
                c.execute("INSERT INTO edge VALUES(?,?,?)",r)
        except Exception as e: return (name,seed,hist+[op],"ERR",str(e))
        hist.append(op)
        from collections import Counter
        fresh=Counter(map(repr,c.execute(q).fetchall())); inc=Counter(map(repr,c.execute("SELECT * FROM v").fetchall()))
        if fresh!=inc:
            fo=fresh-inc; io=inc-fresh
            loose = Counter(k.replace('.0,',',').replace('.0)',')') for k in fo.elements())==Counter(k.replace('.0,',',').replace('.0)',')') for k in io.elements())
            return (name,seed,hist,"DIVERGE" if not loose else "TYPEONLY","fresh-only",sorted(fo.elements()),"inc-only",sorted(io.elements()),c.execute("select * from edge").fetchall(),c.execute("select * from node").fetchall())
    return None
names=sys.argv[1:] or list(QUERIES)
for name in names:
    bad=None; typeonly=None
    for seed in range(int(os.environ.get("SEEDS","120"))):
        r=run(name,seed)
        if r and r[1]=="UNSUPPORTED": bad=r; break
        if r and r[3]!="TYPEONLY": bad=r; break
        if r: typeonly=r
    print(name, ("OK" if not typeonly else ("TYPEONLY",typeonly[1],typeonly[5],typeonly[7])) if not bad else bad)
