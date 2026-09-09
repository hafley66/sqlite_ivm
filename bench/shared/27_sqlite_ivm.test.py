"""Stock loaded extension contracts. Hosts submit SQL; every transition is recomputed."""
import json
import os
from pathlib import Path
import random
import sqlite3
import subprocess
import tempfile
import unittest

EXT = os.environ.get('SQLITE_IVM_EXTENSION', '/private/tmp/sqlite-ivm-astra-target/debug/libsqlite_ivm.dylib')
CLI = os.environ.get('SQLITE_IVM_CLI', '/opt/homebrew/opt/sqlite/bin/sqlite3')
QUERY = 'SELECT f.k AS key,COUNT(*) AS n,SUM(f.v*d.v) AS s FROM items f JOIN weights d ON f.k=d.k GROUP BY f.k'
SCHEMA = 'CREATE TABLE items(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER); CREATE TABLE weights(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER);'

def configure(db, load=False):
    db.execute('PRAGMA recursive_triggers=ON')
    db.execute('PRAGMA trusted_schema=ON')
    if load:
        db.enable_load_extension(True)
        db.load_extension(EXT)
        db.enable_load_extension(False)

def internal(view, suffix):
    return '__ivm_' + view.encode().hex() + '_' + suffix

class Plugin(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='ivm-plugin-')
        self.path = Path(self.temp.name)/'test.sqlite'
        self.db = sqlite3.connect(self.path, isolation_level=None)
        configure(self.db, True)
        self.db.executescript(SCHEMA + 'INSERT INTO items VALUES(1,1,4),(2,1,-4),(3,2,0); INSERT INTO weights VALUES(1,1,2),(2,1,3),(3,2,-2);')
        self.db.execute('SELECT sqlite_ivm_create(?,?)', ('totals', QUERY)).fetchall()

    def tearDown(self):
        self.db.close()
        self.temp.cleanup()

    def exact(self, db=None):
        db = db or self.db
        actual = db.execute('SELECT * FROM totals ORDER BY key').fetchall()
        expected = db.execute(QUERY + ' ORDER BY f.k').fetchall()
        self.assertEqual(actual, expected)
        self.assertEqual(db.execute('SELECT * FROM '+internal('totals','delta')).fetchall(), [])
        return (db.execute('SELECT * FROM items ORDER BY id').fetchall(), db.execute('SELECT * FROM weights ORDER BY id').fetchall(), actual)

    def test_01_bag_join_moves_zero_and_empty(self):
        self.assertEqual(self.exact()[2], [(1,4,0),(2,1,0)])
        for sql in ['UPDATE weights SET k=3-k,v=-v', 'UPDATE items SET k=3-k,v=v+1',
                    'INSERT INTO weights VALUES(4,1,0)', 'DELETE FROM weights WHERE k=2',
                    'DELETE FROM items', 'INSERT INTO items VALUES(1,1,-2)', 'DELETE FROM weights']:
            self.db.execute(sql); self.exact()
        self.assertEqual(self.exact()[2], [])

    def test_02_seeded_inputs_and_oracle(self):
        trace=[]
        expected={t:{r[0]:r for r in self.db.execute('SELECT * FROM '+t)} for t in ['items','weights']}
        for seed in [7,42,2026]:
            rng=random.Random(seed)
            for step in range(80):
                t=rng.choice(['items','weights']); row=(rng.randrange(1,25),rng.randrange(-3,4),rng.randrange(-10,11))
                delete=rng.randrange(4)==0
                sql='DELETE FROM '+t+' WHERE id=?' if delete else 'INSERT INTO '+t+' VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET k=excluded.k,v=excluded.v'
                params=(row[0],) if delete else row
                trace.append([seed,step,sql,params])
                try:
                    self.db.execute(sql,params)
                    if delete: expected[t].pop(row[0],None)
                    else: expected[t][row[0]]=row
                    state=self.exact()
                    self.assertEqual(state[:2],tuple(sorted(expected[t].values()) for t in ['items','weights']))
                except Exception:
                    destination=Path(os.environ.get('SQLITE_IVM_FAILURE_ROOT',self.temp.name))
                    destination.mkdir(parents=True,exist_ok=True)
                    (destination/'seeded-failure.json').write_text(json.dumps(trace))
                    raise

    def test_03_transaction_savepoint(self):
        before=self.exact()
        self.db.execute('BEGIN')
        self.db.execute('UPDATE items SET v=7'); self.exact()
        self.db.execute('SAVEPOINT nested')
        self.db.execute('DELETE FROM weights'); self.exact()
        self.db.execute('ROLLBACK TO nested'); self.exact()
        self.db.execute('ROLLBACK')
        self.assertEqual(self.exact(),before)

    def test_04_conflict_policies(self):
        for table in ['items','weights']:
            for policy in ['REPLACE','IGNORE','FAIL','ABORT','ROLLBACK']:
                with self.subTest(table=table,policy=policy):
                    self.db.execute('BEGIN')
                    before=self.exact()
                    try: self.db.execute(f'INSERT OR {policy} INTO {table} VALUES(50,1,8),(1,2,9)')
                    except sqlite3.IntegrityError: pass
                    self.exact()
                    if policy in ['ABORT','ROLLBACK']: self.assertEqual(self.exact(),before)
                    if policy=='FAIL': self.assertEqual(self.db.execute(f'SELECT * FROM {table} WHERE id=50').fetchall(),[(50,1,8)])
                    if self.db.in_transaction: self.db.execute('ROLLBACK')
        self.db.execute('UPDATE OR REPLACE weights SET id=2,k=3 WHERE id=1'); self.exact()

    def test_05_reopen_second_connection_no_extension(self):
        self.db.execute('PRAGMA journal_mode=WAL')
        self.db.close(); self.db=sqlite3.connect(self.path,isolation_level=None); configure(self.db)
        second=sqlite3.connect(self.path,isolation_level=None)
        try:
            configure(second)
            second.execute('UPDATE weights SET v=-3'); self.exact()
            self.db.execute('BEGIN IMMEDIATE')
            self.db.execute('UPDATE items SET v=2'); self.exact()
            second.execute('PRAGMA busy_timeout=1')
            with self.assertRaisesRegex(sqlite3.OperationalError,'locked'): second.execute('UPDATE items SET v=4')
            self.db.execute('ROLLBACK'); self.exact(second)
        finally: second.close()

    def test_06_settings_fail_closed(self):
        for setting in ['recursive_triggers','trusted_schema']:
            before=self.exact(); self.db.execute(f'PRAGMA {setting}=OFF')
            with self.assertRaises(sqlite3.Error): self.db.execute('INSERT OR REPLACE INTO items VALUES(1,1,99)')
            self.db.execute(f'PRAGMA {setting}=ON'); self.assertEqual(self.exact(),before)

    def test_07_numeric_affinity_null_guards(self):
        for value in [None,1.5,'abc',b'x',1000001,-1000001,9223372036854775807]:
            for policy in ['ABORT','IGNORE','REPLACE','FAIL']:
                before=self.exact()
                with self.assertRaises(sqlite3.Error): self.db.execute(f'INSERT OR {policy} INTO items VALUES(100,1,?)',(value,))
                self.assertEqual(self.exact(),before)
        self.db.execute("INSERT INTO items VALUES(100,'1','7')"); self.exact()
        self.db.execute('UPDATE items SET v=1000000'); self.db.execute('UPDATE weights SET v=-1000000'); self.exact()

    def test_08_rejection_matrix_no_schema_mutation(self):
        cases={
            'projection':'SELECT k FROM items',
            'filter':QUERY.replace(' GROUP BY',' WHERE f.v>0 GROUP BY'),
            'distinct':QUERY.replace('SELECT','SELECT DISTINCT',1),
            'count_distinct':QUERY.replace('COUNT(*)','COUNT(DISTINCT f.v)'),
            'self':QUERY.replace('weights d','items d'),
            'multiway':QUERY.replace(' GROUP BY',' JOIN weights e ON e.k=f.k GROUP BY'),
            'left':QUERY.replace(' JOIN',' LEFT JOIN'),
            'right':QUERY.replace(' JOIN',' RIGHT JOIN'),
            'full':QUERY.replace(' JOIN',' FULL JOIN'),
            'semi':QUERY.replace(' GROUP BY',' WHERE EXISTS(SELECT 1 FROM items) GROUP BY'),
            'anti':QUERY.replace(' GROUP BY',' WHERE NOT EXISTS(SELECT 1 FROM items) GROUP BY'),
            'union':QUERY+' UNION '+QUERY,
            'union_all':QUERY+' UNION ALL '+QUERY,
            'except':QUERY+' EXCEPT '+QUERY,
            'intersect':QUERY+' INTERSECT '+QUERY,
            'avg':QUERY.replace('SUM','AVG'), 'min':QUERY.replace('SUM','MIN'), 'max':QUERY.replace('SUM','MAX'),
            'global':QUERY.replace('f.k AS key,','').replace(' GROUP BY f.k',''),
            'subquery':QUERY.replace('items f','(SELECT * FROM items) f'),
            'cte':'WITH x AS(SELECT * FROM items) '+QUERY,
            'recursive':'WITH RECURSIVE x(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<3) '+QUERY,
            'order':QUERY+' ORDER BY f.k', 'topk':QUERY+' LIMIT 1',
            'window':QUERY.replace('COUNT(*)','COUNT(*) OVER ()'),
            'having':QUERY+' HAVING COUNT(*)>1',
            'float':QUERY.replace('f.v*d.v','f.v*1.5'),
            'overflow_expr':QUERY.replace('f.v*d.v','f.v*d.v*f.v'),
            'function':QUERY.replace('f.v*d.v','random()'),
            'collate':QUERY.replace('f.k=d.k','f.k COLLATE NOCASE=d.k'),
            'unbound':QUERY.replace('f.v*d.v','absent*d.v'),
            'injection':QUERY+'; DROP TABLE items;',
        }
        before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
        for case,sql in cases.items():
            with self.subTest(case=case):
                with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('bad',sql)).fetchall()
                self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)
        self.assertEqual(len(cases),32)

    def test_09_atomic_install_drop_multiple_collisions(self):
        before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
        self.db.execute('BEGIN')
        self.db.execute('SELECT sqlite_ivm_create(?,?)',('other',QUERY)).fetchall()
        self.db.execute('UPDATE items SET v=2')
        self.assertEqual(self.db.execute('SELECT * FROM other').fetchall(),self.db.execute('SELECT * FROM totals').fetchall())
        self.db.execute("SELECT sqlite_ivm_drop('other')").fetchall()
        self.db.execute('ROLLBACK')
        self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)
        with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('totals',QUERY)).fetchall()
        self.db.execute('CREATE TABLE '+internal('bad','delta')+'(x)')
        with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('bad',QUERY)).fetchall()
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name='bad'").fetchall(),[])
        self.db.execute("SELECT sqlite_ivm_drop('totals')").fetchall()
        self.db.execute('UPDATE items SET v=3')
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name LIKE '__ivm_%' AND name NOT LIKE '__ivm_626164%'").fetchall(),[])

    def test_10_failed_ddl_cleanup_and_caller_preserved(self):
        # Authorizer denies a late CREATE INDEX after tables/population/view exist.
        before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
        self.db.execute('BEGIN'); self.db.execute('UPDATE items SET v=8')
        state=self.exact()
        self.db.set_authorizer(lambda action,*args: sqlite3.SQLITE_DENY if action==sqlite3.SQLITE_CREATE_INDEX else sqlite3.SQLITE_OK)
        try:
            with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('late',QUERY)).fetchall()
        finally: self.db.set_authorizer(None)
        self.assertTrue(self.db.in_transaction)
        self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)
        self.assertEqual(self.exact(),state); self.db.execute('ROLLBACK')

    def test_11_renamed_quoted_names_and_using(self):
        self.db.executescript('CREATE TABLE "odd table"("group key" INTEGER,"value" INTEGER); CREATE TABLE "other table"("group key" INTEGER,"factor" INTEGER); INSERT INTO "odd table" VALUES(1,2),(1,2); INSERT INTO "other table" VALUES(1,3),(1,4);')
        query='SELECT "group key",count(*) AS "row count",sum(a.value*b.factor) AS "total sum" FROM "odd table" a JOIN "other table" b USING("group key") GROUP BY "group key"'
        name='a"; DROP TABLE items; --'
        self.db.execute('SELECT sqlite_ivm_create(?,?)',(name,query)).fetchall()
        quoted='"'+name.replace('"','""')+'"'
        self.assertEqual(self.db.execute('SELECT * FROM '+quoted).fetchall(),[(1,4,28)])
        self.db.execute('UPDATE "other table" SET factor=-factor')
        self.assertEqual(self.db.execute('SELECT * FROM '+quoted).fetchall(),[(1,4,-28)])
        self.db.execute('SELECT sqlite_ivm_drop(?)',(name,)).fetchall(); self.exact()

    def test_12_storage_variants_and_catalog_rejections(self):
        for suffix in [' STRICT',' WITHOUT ROWID',' STRICT, WITHOUT ROWID','']:
            self.db.execute('CREATE TABLE extra(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER)'+suffix)
            q=QUERY.replace('items f','extra f')
            self.db.execute('SELECT sqlite_ivm_create(?,?)',('variant',q)).fetchall()
            self.db.execute('INSERT INTO extra VALUES(1,1,4)')
            self.assertEqual(self.db.execute('SELECT * FROM variant').fetchall(),[(1,2,20)])
            self.db.execute("SELECT sqlite_ivm_drop('variant')").fetchall(); self.db.execute('DROP TABLE extra')
        for declaration in ['TEXT','REAL','INTEGER COLLATE NOCASE','INTEGER GENERATED ALWAYS AS(1)']:
            self.db.execute('CREATE TABLE extra(id INTEGER,k INTEGER,v '+declaration+')')
            with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('variant',QUERY.replace('items f','extra f'))).fetchall()
            self.db.execute('DROP TABLE extra')

    def test_13_telemetry_counters_rollback_second_writer_disabled(self):
        meta=internal('totals','meta')
        self.db.execute('UPDATE items SET v=5'); self.exact()
        self.assertEqual(self.db.execute('SELECT operations,contributions,groups_touched FROM '+meta).fetchone(),(0,0,0))
        self.db.execute("SELECT sqlite_ivm_metrics('totals',1)").fetchall()
        self.db.execute('UPDATE items SET v=6 WHERE id=1'); self.exact()
        counters=self.db.execute('SELECT * FROM '+meta).fetchall()
        self.assertEqual(counters,[(1,1,1,1,4,2)])
        self.db.execute('BEGIN'); self.db.execute('DELETE FROM items'); self.db.execute('ROLLBACK')
        self.assertEqual(self.db.execute('SELECT * FROM '+meta).fetchall(),counters)
        other=sqlite3.connect(self.path,isolation_level=None)
        try:
            configure(other); other.execute('UPDATE weights SET v=9 WHERE id=1'); self.exact()
        finally: other.close()
        self.assertEqual(self.db.execute('SELECT operations FROM '+meta).fetchone(),(2,))

    def test_14_stock_cli_load_diagnostics_and_disabled(self):
        script=f'.bail on\n.load {EXT}\nPRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;\n'+SCHEMA+"\nSELECT sqlite_ivm_create('v', '"+QUERY.replace("'","''")+"');\nSELECT sqlite_ivm_drop('v');\n"
        run=subprocess.run([CLI,':memory:'],input=script,text=True,capture_output=True,timeout=30,env={**os.environ,'SQLITE_IVM_LOG_LIMIT':'20'})
        self.assertEqual(run.returncode,0,run.stderr); self.assertEqual(run.stdout,'v\nv\n')
        events=[json.loads(line)['fields'] for line in run.stderr.splitlines()]
        self.assertEqual([e['boundary'] for e in events],['load','install','bind','lower','schema','install_release','drop_release'])
        run=subprocess.run([CLI,':memory:'],input=script,text=True,capture_output=True,timeout=30,env={**os.environ,'SQLITE_IVM_LOG_LIMIT':'0'})
        self.assertEqual((run.returncode,run.stdout,run.stderr),(0,'v\nv\n',''))
        rejected=f'.load {EXT}\n'+SCHEMA+"\nSELECT sqlite_ivm_create('v','SELECT 1');\n"
        run=subprocess.run([CLI,':memory:'],input=rejected,text=True,capture_output=True,timeout=30,env={**os.environ,'SQLITE_IVM_LOG_LIMIT':'3'})
        events=[json.loads(line)['fields'] for line in run.stderr.splitlines() if line.startswith('{')]
        self.assertEqual([e['boundary'] for e in events],['load','install','install_rollback'])
        self.assertEqual(events[-1]['extended_code'],1)

    def test_15_injected_corruption_detector(self):
        self.db.execute('UPDATE '+internal('totals','acc')+' SET s=s+1 WHERE k=1')
        with self.assertRaises(AssertionError): self.exact()

    def test_16_public_result_is_read_only(self):
        with self.assertRaises(sqlite3.Error): self.db.execute('UPDATE totals SET s=99')
        self.exact()

    def test_17_aggregate_cap_rejects_without_partial_state(self):
        self.db.executescript('CREATE TABLE many(k INTEGER,v INTEGER); CREATE TABLE more(k INTEGER,v INTEGER); WITH RECURSIVE x(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM x WHERE i<1000) INSERT INTO many SELECT 1,1000000 FROM x; INSERT INTO more SELECT * FROM many;')
        query='SELECT a.k,count(*) AS n,sum(a.v*b.v) AS s FROM many a JOIN more b ON a.k=b.k GROUP BY a.k'
        self.db.execute('SELECT sqlite_ivm_create(?,?)',('cap',query)).fetchall()
        self.assertEqual(self.db.execute('SELECT * FROM cap').fetchall(),[(1,1000000,1000000000000000000)])
        for policy in ['FAIL','IGNORE','REPLACE','ABORT']:
            with self.assertRaises(sqlite3.Error): self.db.execute('INSERT OR '+policy+' INTO many VALUES(1,1000000)')
            self.assertEqual(self.db.execute('SELECT count(*) FROM many').fetchone(),(1000,))
            self.assertEqual(self.db.execute('SELECT * FROM cap').fetchall(),[(1,1000000,1000000000000000000)])
        self.db.execute("SELECT sqlite_ivm_drop('cap')").fetchall()
        self.db.execute('INSERT INTO many VALUES(1,1000000)')
        with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('cap',query)).fetchall()
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name='cap'").fetchall(),[])

    def test_18_catalog_shadow_and_unmanaged_triggers(self):
        self.db.execute('CREATE TEMP TABLE items(id INTEGER,k INTEGER,v INTEGER)')
        with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('shadow',QUERY)).fetchall()
        self.db.execute('DROP TABLE temp.items')
        self.db.execute('CREATE TRIGGER custom AFTER INSERT ON items BEGIN SELECT 1; END')
        with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('custom',QUERY)).fetchall()

    def test_19_telemetry_sink_failure_does_not_fail_install(self):
        target=Path(self.temp.name)/'sink.sqlite'
        script=f'.bail on\n.load {EXT}\n'+SCHEMA+"\nSELECT sqlite_ivm_create('v','"+QUERY+"');\n"
        run=subprocess.run([CLI,str(target)],input=script,text=True,stdout=subprocess.PIPE,
            preexec_fn=lambda:os.close(2),env={**os.environ,'SQLITE_IVM_LOG_LIMIT':'10'},timeout=30)
        self.assertEqual((run.returncode,run.stdout),(0,'v\n'))
        check=sqlite3.connect(target)
        try: self.assertEqual(check.execute('SELECT * FROM v').fetchall(),[])
        finally: check.close()

    def test_20_table_foreign_key_cascade_rejected_before_install(self):
        self.db.execute('CREATE TABLE child(id INTEGER PRIMARY KEY,k INTEGER,v INTEGER,FOREIGN KEY(k) REFERENCES weights(id) ON UPDATE CASCADE)')
        before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
        with self.assertRaisesRegex(sqlite3.Error,'foreign keys unsupported'):
            self.db.execute('SELECT sqlite_ivm_create(?,?)',('cascade',QUERY.replace('items f','child f'))).fetchall()
        self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)

    def test_21_initial_values_rejected_atomically(self):
        self.db.execute('CREATE TABLE nullable(id INTEGER,k INTEGER,v INTEGER)')
        query=QUERY.replace('items f','nullable f')
        for value in [None,1.5,1000001]:
            self.db.execute('DELETE FROM nullable')
            self.db.execute('INSERT INTO nullable VALUES(1,1,?)',(value,))
            before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
            with self.assertRaises(sqlite3.Error): self.db.execute('SELECT sqlite_ivm_create(?,?)',('invalid',query)).fetchall()
            self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)

    def test_22_failed_drop_restores_objects(self):
        before=self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall()
        self.db.set_authorizer(lambda action,*args:sqlite3.SQLITE_DENY if action==sqlite3.SQLITE_DROP_TABLE else sqlite3.SQLITE_OK)
        try:
            with self.assertRaises(sqlite3.Error): self.db.execute("SELECT sqlite_ivm_drop('totals')").fetchall()
        finally: self.db.set_authorizer(None)
        self.assertEqual(self.db.execute('SELECT * FROM sqlite_schema ORDER BY name').fetchall(),before)
        self.db.execute('UPDATE items SET v=9'); self.exact()

    def test_23_host_trace_ownership_and_directonly(self):
        other=sqlite3.connect(':memory:',isolation_level=None)
        observed=[]
        try:
            other.set_trace_callback(lambda sql:observed.append(sql) if sql=='SELECT 42' else None)
            configure(other,True)
            other.execute('SELECT 42').fetchall()
            self.assertEqual(observed,['SELECT 42'])
            other.execute('CREATE VIEW indirect AS SELECT sqlite_ivm_version()')
            with self.assertRaises(sqlite3.Error): other.execute('SELECT * FROM indirect').fetchall()
        finally:other.close()

    def test_24_assigned_integer_primary_key_bound(self):
        self.db.execute('INSERT INTO items VALUES(1000000,1,7)')
        before=self.exact()
        for policy in ['ABORT','FAIL','IGNORE','REPLACE']:
            with self.assertRaisesRegex(sqlite3.Error,'assigned row outside integer bound'):
                self.db.execute('INSERT OR '+policy+' INTO items(k,v) VALUES(1,8)')
            self.assertEqual(self.exact(),before)

if __name__=='__main__': unittest.main()
