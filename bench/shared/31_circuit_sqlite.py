"""Full-query baseline and actual plugin admission against shared circuit fixtures."""
import argparse
import hashlib
import json
import os
import sqlite3
import sys
import time


def emit(**record):
    print(json.dumps(record), flush=True)


def canonical(rows, prefix):
    return ''.join(prefix + '\t' + '\t'.join(str(n) for n in row) + '\n' for row in sorted(rows))


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--fixture', required=True)
    p.add_argument('--db', required=True)
    p.add_argument('--extension', default='')
    args = p.parse_args()
    fixture = json.load(open(args.fixture))
    db = sqlite3.connect(args.db, isolation_level=None)
    start = time.perf_counter()
    db.execute('PRAGMA journal_mode=WAL')
    db.execute('PRAGMA synchronous=FULL')
    db.execute('PRAGMA recursive_triggers=ON')
    for table in ('a', 'b', 'c'):
        db.execute(f'CREATE TABLE {table}(id INTEGER PRIMARY KEY,k INTEGER NOT NULL,v INTEGER NOT NULL)')
        db.execute(f'CREATE INDEX {table}_k ON {table}(k)')
        db.execute(f'CREATE INDEX {table}_v ON {table}(v)')
    query = fixture['query']
    if args.extension:
        db.enable_load_extension(True)
        db.load_extension(args.extension)
        db.enable_load_extension(False)
        try:
            db.execute('SELECT sqlite_ivm_create(?,?)', ('circuit_view', query)).fetchone()
        except sqlite3.Error as error:
            # Rejection must leave no partial installation. Only explicitly
            # inventoried non-admitted families may produce a capability row.
            if fixture['circuit'] == 'aggregate_churn' or str(error) not in (
                'sqlite_ivm: grouped SELECT only; filter/distinct/global/window unsupported',
                'sqlite_ivm: CTE/order/limit/set operations unsupported',
            ):
                raise
            assert db.execute("SELECT count(*) FROM sqlite_schema WHERE name LIKE '__ivm_%' OR name='circuit_view'").fetchone()[0] == 0
            emit(event='capability', status='unsupported', reason=str(error),
                 sqlite_extended_error_code=getattr(error, 'sqlite_errorcode', None),
                 installation_atomic=True, circuit=fixture['circuit'])
            db.close()
            return
        query = 'SELECT * FROM circuit_view'
    emit(event='case-setup', status='ok', setup_ms=(time.perf_counter()-start)*1000,
         sqlite_version=sqlite3.sqlite_version, algorithm='sql-trigger-arithmetic' if args.extension else 'full-query',
         durability='WAL synchronous FULL', logging_limit=os.environ.get('SQLITE_IVM_LOG_LIMIT', '0'))
    log_limit = min(32, max(0, int(os.environ.get('SQLITE_IVM_LOG_LIMIT', '0'))))
    if args.extension:
        db.execute('SELECT sqlite_ivm_metrics(?,?)', ('circuit_view', int(log_limit > 0))).fetchone()
    total = 0
    for state in fixture['states']:
        start = time.perf_counter()
        db.executescript('BEGIN;\n' + state['mutation_sql'] + '\nCOMMIT;')
        update_ms = (time.perf_counter()-start)*1000
        start = time.perf_counter()
        output = sorted([list(row) for row in db.execute(query)])
        query_ms = (time.perf_counter()-start)*1000
        inputs = {t: sorted([list(row) for row in db.execute(f'SELECT id,k,v FROM {t}')]) for t in ('a','b','c')}
        assert inputs == {t: sorted(rows) for t, rows in state['inputs'].items()}, state['name'] + ': inputs'
        assert output == state['expected']['rows'], state['name'] + ': output'
        oracle = sorted([list(row) for row in db.execute(fixture['query'])])
        assert output == oracle, state['name'] + ': SQL recompute'
        input_hash = digest(''.join(canonical(inputs[t], t.upper()) for t in ('a','b','c')))
        checksum = digest(canonical(output, 'S'))
        assert input_hash == state['input_hash'] and checksum == state['expected']['checksum']
        total += update_ms + query_ms
        if args.extension and log_limit > 0:
            meta = '__ivm_' + 'circuit_view'.encode().hex() + '_meta'
            metrics = db.execute(f'SELECT operations,contributions,groups_touched FROM "{meta}"').fetchone()
            try:
                print(json.dumps({'event':'circuit-maintenance-observed','view':'circuit_view','operation':state['name'],
                      'duration_ms':update_ms,'cumulative_operations':metrics[0],'cumulative_contributions':metrics[1],
                      'cumulative_groups_touched':metrics[2],'row_values':'redacted','sqlite_extended_error_code':0}),file=sys.stderr)
            except OSError:
                pass
            log_limit -= 1
        emit(event='mutation', status='ok', state=state['name'], exact_input_output_validated=True,
             input_hash=input_hash, checksum=checksum, affected_rows=len(state['writes']), output_rows=len(output),
             output_bytes=len(canonical(output,'S').encode()), update_transaction_ms=update_ms,
             query_compute_ms=query_ms, update_plus_query_ms=update_ms+query_ms)
    emit(event='case-total', status='ok', update_plus_query_ms=total, final_input_hash=input_hash,
         final_checksum=checksum, disk={'database_bytes':os.path.getsize(args.db),
         'wal_bytes':os.path.getsize(args.db+'-wal') if os.path.exists(args.db+'-wal') else 0})
    db.close()


if __name__ == '__main__':
    main()
