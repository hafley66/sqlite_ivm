#!/usr/bin/env python3
"""Count stored copies of one probe source row across every table sqlite_ivm owns.

Usage: measure.py <libsqlite_ivm.dylib> [fanout ...]

Circuit: SELECT a.k, SUM(b.w) FROM a JOIN b ON a.k=b.k WHERE a.v>0 GROUP BY a.k
Rows: 2000 in a, 2000 in b. k = id % (2000/fanout), so a.id=1 matches `fanout` rows of b.
Probe row: a.id=1 has v=1000001. No other cell in a or b holds that value.
A copy is any stored cell whose integer value is 1000001, or whose text contains 1000001.
"""
import json
import os
import subprocess
import sys
import tempfile

SQLITE3 = os.environ.get('SQLITE3', '/opt/homebrew/opt/sqlite/bin/sqlite3')
MARK = 1000001
ROWS = 2000
VIEW = 'SELECT a.k AS k, SUM(b.w) AS total FROM a JOIN b ON a.k=b.k WHERE a.v>0 GROUP BY a.k'


def run(db, lib, sql):
    script = f".load {lib}\nPRAGMA recursive_triggers=ON;\nPRAGMA trusted_schema=ON;\n.mode json\n{sql}\n"
    out = subprocess.run([SQLITE3, db], input=script, capture_output=True, text=True, check=True)
    text = out.stdout.strip()
    return json.loads(text) if text else []


def build(db, lib, fanout):
    groups = ROWS // fanout
    run(db, lib, f"""
CREATE TABLE a(id INTEGER PRIMARY KEY, k INTEGER, v INTEGER);
CREATE TABLE b(id INTEGER PRIMARY KEY, k INTEGER, w INTEGER);
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{ROWS})
INSERT INTO a SELECT i, i%{groups}, {MARK - 1}+i FROM s;
WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{ROWS})
INSERT INTO b SELECT i, i%{groups}, 2000000+i FROM s;
SELECT sqlite_ivm_create('chain','{VIEW}');
""")


def inventory(db, lib):
    tables = run(db, lib, "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;")
    report = []
    for row in tables:
        t = row['name']
        cols = [c['name'] for c in run(db, lib, f'PRAGMA table_info("{t}");')]
        if not cols:
            continue
        cells = ' + '.join(
            f"sum(CASE WHEN typeof(\"{c}\")='integer' THEN \"{c}\"={MARK} WHEN typeof(\"{c}\")='text' THEN instr(\"{c}\",'{MARK}')>0 ELSE 0 END)"
            for c in cols)
        per_col = ', '.join(
            f"sum(CASE WHEN typeof(\"{c}\")='integer' THEN \"{c}\"={MARK} WHEN typeof(\"{c}\")='text' THEN instr(\"{c}\",'{MARK}')>0 ELSE 0 END) AS \"{c}\""
            for c in cols)
        r = run(db, lib, f'SELECT count(*) AS rows, {cells} AS copies, {per_col} FROM "{t}";')[0]
        size = run(db, lib, f"SELECT coalesce(sum(pgsize),0) AS bytes FROM dbstat WHERE name='{t}' OR name IN (SELECT name FROM sqlite_schema WHERE type='index' AND tbl_name='{t}');")[0]['bytes']
        by_col = {c: r[c] for c in cols if r[c]}
        report.append({'table': t, 'columns': cols, 'rows': r['rows'], 'copies': r['copies'] or 0, 'by_column': by_col, 'bytes': size})
    return report


def main():
    lib = os.path.abspath(sys.argv[1])
    fanouts = [int(x) for x in sys.argv[2:]] or [1, 10, 40, 100]
    for fanout in fanouts:
        with tempfile.TemporaryDirectory() as d:
            db = os.path.join(d, 'probe.db')
            build(db, lib, fanout)
            rows = inventory(db, lib)
            total = sum(r['copies'] for r in rows)
            file_bytes = os.path.getsize(db)
            print(json.dumps({'lib': lib, 'fanout': fanout, 'total_copies': total, 'file_bytes': file_bytes, 'tables': rows}))


if __name__ == '__main__':
    main()
