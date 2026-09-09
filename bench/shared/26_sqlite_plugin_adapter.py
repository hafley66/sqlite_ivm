"""Loaded plugin arm using the existing fixture/timers/oracle transport.

The host installs and submits ordinary SQL writes. SQL triggers do maintenance.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path

LAB=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('template',LAB/'19_sqlite_template_adapter.py')
template=importlib.util.module_from_spec(spec)
spec.loader.exec_module(template)

QUERY='SELECT f.group_id,COUNT(*) AS n,SUM(f.amount*d.factor) AS s FROM fact f JOIN dimension d ON f.group_id=d.group_id GROUP BY f.group_id'

def install(db,initial,extension,metrics,sql_path):
    db.execute('PRAGMA trusted_schema=ON')
    db.execute('PRAGMA recursive_triggers=ON')
    db.execute('PRAGMA journal_mode=WAL')
    db.execute('PRAGMA synchronous=FULL')
    db.enable_load_extension(True)
    db.load_extension(extension)
    db.enable_load_extension(False)
    db.executescript('CREATE TABLE fact(id INTEGER PRIMARY KEY,group_id INTEGER NOT NULL,amount INTEGER NOT NULL); CREATE TABLE dimension(group_id INTEGER PRIMARY KEY,factor INTEGER NOT NULL);')
    db.execute('BEGIN IMMEDIATE')
    for rel,rows in initial.items(): db.executemany('INSERT INTO '+rel+' VALUES('+','.join('?' for _ in rows[0])+')',rows)
    db.execute('COMMIT')
    db.execute('SELECT sqlite_ivm_create(?,?)',('summary',QUERY)).fetchall()
    db.execute('SELECT sqlite_ivm_metrics(?,?)',('summary',metrics)).fetchall()
    if sql_path:
        Path(sql_path).write_text(';\n'.join(r[0] for r in db.execute('SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY rowid'))+';\n')
    return {'algorithm':'signed_join_count_sum persistent SQL triggers','extension_sha256':hashlib.sha256(Path(extension).read_bytes()).hexdigest(),
            'extension_path':str(Path(extension).resolve()),'build':json.loads(db.execute('SELECT sqlite_ivm_version()').fetchone()[0]),
            'logging':{'event_limit':int(os.environ.get('SQLITE_IVM_LOG_LIMIT','0')),'transactional_metrics':metrics,'sink':'stderr'},
            'durability':'WAL/synchronous=FULL','source_foreign_key':False}

if __name__=='__main__': template.main(install)
