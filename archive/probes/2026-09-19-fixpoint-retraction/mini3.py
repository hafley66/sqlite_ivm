import sqlite3, random, os
from collections import Counter
EXT="/Users/chrishafley/projects/sqlite_ivm/target/release/libsqlite_ivm.dylib"
Q="WITH RECURSIVE path(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,e.b FROM path p JOIN edge e ON e.a=p.y) SELECT x,y FROM path"
def val(rnd,nv):
    r=rnd.random()
    if r<0.1: return None
    if r<0.2: return float(rnd.randint(1,nv))
    if r<0.3: return str(rnd.randint(1,nv))
    return rnd.randint(1,nv)
def gen(seed,nv=6,steps=50):
    rnd=random.Random(seed); init=[]
    for _ in range(rnd.randint(0,10)): init.append((val(rnd,nv),val(rnd,nv)))
    ops=[]; rows=list(init)
    for _ in range(steps):
        if rows and rnd.random()<0.5:
            r=rows.pop(rnd.randrange(len(rows))); ops.append(("D",r))
        else:
            r=(val(rnd,nv),val(rnd,nv)); rows.append(r); ops.append(("I",r))
    return init,ops
def attempt(init, ops):
    c=sqlite3.connect(":memory:"); c.enable_load_extension(True); c.load_extension(EXT)
    c.executescript("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON; CREATE TABLE edge(a,b);")
    for r in init: c.execute("INSERT INTO edge VALUES(?,?)",r)
    c.execute(f"CREATE VIRTUAL TABLE v USING sqlite_ivm('{Q}')")
    for op,r in ops:
        try:
            if op=="I": c.execute("INSERT INTO edge VALUES(?,?)",r)
            else: c.execute("DELETE FROM edge WHERE rowid IN (SELECT rowid FROM edge WHERE typeof(a)=typeof(?1) AND a IS ?1 AND typeof(b)=typeof(?2) AND b IS ?2 LIMIT 1)",r)
        except Exception as e: return "ERR "+str(e)
        f=Counter(map(repr,c.execute(Q))); i=Counter(map(repr,c.execute("select * from v")))
        if f!=i:
            fo=sorted((f-i).elements()); io=sorted((i-f).elements())
            norm=lambda l: sorted(k.replace('.0,',',').replace('.0)',')') for k in l)
            if norm(fo)!=norm(io): return ("DIV",fo,io)
    return None
found=None
for seed in range(1500):
    init,ops=gen(seed)
    r=attempt(init,ops)
    if r and (not isinstance(r,str) and r[0]=="DIV"):
        found=(seed,init,ops,r); break
print("found",found and (found[0],found[3]))
if found:
    seed,init,ops,r=found
    # minimize ops by removing one at a time
    changed=True
    while changed:
        changed=False
        for i in range(len(ops)):
            trial=ops[:i]+ops[i+1:]
            rr=attempt(init,trial)
            if rr and (not isinstance(rr,str) and rr[0]=="DIV"):
                ops=trial; changed=True; break
        for i in range(len(init)):
            trial=init[:i]+init[i+1:]
            rr=attempt(trial,ops)
            if rr and (not isinstance(rr,str) and rr[0]=="DIV"):
                init=trial; changed=True; break
    print("init",init); print("ops",ops); print("result",attempt(init,ops))
