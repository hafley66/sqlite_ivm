import sqlite3, random, struct, math
from collections import Counter
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
def conn():
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;"); return c
vals=[0.1,1/3,1e-300,5e-324,-0.0,0.0,1e300,2**53+1,-2**63,2**63-1,math.pi,1e16,123456789012345678,'x','',b'\x00\x01',None,float('inf'),-float('inf'),1.7976931348623157e308]
rnd=random.Random(1)
for _ in range(2000):
    vals.append(struct.unpack('d',struct.pack('Q',rnd.getrandbits(64)))[0])
vals=[v for v in vals if not (isinstance(v,float) and math.isnan(v))]
for q,label in [("SELECT a,b FROM t","map"),("SELECT DISTINCT a FROM t","dist"),("SELECT a,count(*) AS n FROM t GROUP BY a","grp"),("SELECT x.a AS a,y.b AS b FROM t x JOIN t y ON y.a=x.b","join"),("WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p","fix")]:
    c=conn(); c.execute("CREATE TABLE t(a,b)")
    c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{q}')")
    bad=[]
    for i,v in enumerate(vals):
        w=vals[(i*7+3)%len(vals)]
        try:
            c.execute("INSERT INTO t VALUES(?,?)",(v,w))
            c.execute("DELETE FROM t WHERE rowid=(SELECT max(rowid) FROM t)")
            f=Counter(map(repr,c.execute(q))); inc=Counter(map(repr,c.execute("select * from v")))
            if f!=inc: bad.append((v,w,sorted((f-inc).elements())[:3],sorted((inc-f).elements())[:3]))
        except Exception as e: bad.append((v,w,"ERR",str(e)))
        if len(bad)>3: break
    print(label, "OK" if not bad else bad)
