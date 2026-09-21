import sqlite3, random
from collections import Counter
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
Q="WITH RECURSIVE p(x,y) AS (SELECT a,b FROM t UNION SELECT p.x,t.b FROM p JOIN t ON t.a=p.y) SELECT x,y FROM p"
def attempt(init,ops,dump=False):
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON; CREATE TABLE t(a TEXT COLLATE NOCASE,b TEXT COLLATE NOCASE)")
    for r in init: c.execute("INSERT INTO t VALUES(?,?)",r)
    c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{Q}')")
    for op,r in ops:
        try:
            if op=="I": c.execute("INSERT INTO t VALUES(?,?)",r)
            else: c.execute("DELETE FROM t WHERE rowid IN (SELECT rowid FROM t WHERE a=? COLLATE BINARY AND b=? COLLATE BINARY LIMIT 1)",r)
        except Exception as e:
            if dump: print("  op",op,r,"ERR",e); print("  all:",c.execute("select __k,c0,c1 from v_op5_2").fetchall()); print("  state:",c.execute("select __key,c0,c1 from v_state").fetchall())
            return "ERR"
        if dump: print("  op",op,r,"all:",c.execute("select __k,c0,c1 from v_op5_2").fetchall(),"state:",c.execute("select c0,c1 from v_state").fetchall())
    return None
rnd=random.Random(0); v=lambda: rnd.choice(['a','A','b','B','c','C','d'])
init=[(v(),v()) for _ in range(rnd.randint(1,6))]
ops=[('I', ('B', 'b')), ('I', ('A', 'c')), ('D', ('a', 'b')), ('I', ('c', 'd')), ('D', ('A', 'c'))]
print("init",init, attempt(init,ops))
changed=True
while changed:
    changed=False
    for i in range(len(ops)):
        t=ops[:i]+ops[i+1:]
        if attempt(init,t)=="ERR": ops=t; changed=True; break
    for i in range(len(init)):
        t=init[:i]+init[i+1:]
        if attempt(t,ops)=="ERR": init=t; changed=True; break
print("minimal init",init,"ops",ops)
attempt(init,ops,dump=True)
