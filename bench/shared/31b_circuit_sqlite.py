"""Full-query baseline and actual plugin admission against shared circuit fixtures."""
import argparse
import hashlib
import json
import os
import sqlite3
import sys
import time
from functools import cmp_to_key
from importlib import import_module

sqlite_inventory = import_module('30a_state_inventory').sqlite_inventory


def emit(**record):
    print(json.dumps(record), flush=True)


def canonical(rows, prefix, domain):
    ordered = sorted(rows) if domain == 'integers' else rows
    return ''.join(prefix + '\t' + '\t'.join(str(n) for n in row) + '\n' for row in ordered)


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def normalized(rows, domain):
    values = [list(row) for row in rows] if domain != 'mixed_int_real' else [[f'{cell:.1f}' if isinstance(cell, float) and cell.is_integer() else cell for cell in row] for row in rows]
    if domain == 'integers':
        return sorted(values)
    def compare(left, right):
        for a, b in zip(left, right):
            order = (a > b) - (a < b) if isinstance(a, (int, float)) and isinstance(b, (int, float)) else (str(a) > str(b)) - (str(a) < str(b))
            if order:
                return order
        return 0
    return sorted(values, key=cmp_to_key(compare))


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--fixture', required=True)
    p.add_argument('--db', required=True)
    p.add_argument('--extension', default='')
    args = p.parse_args()
    fixture = json.load(open(args.fixture))
    domain = fixture.get('value_domain', 'integers')
    db = sqlite3.connect(args.db, isolation_level=None)
    start = time.perf_counter()
    db.execute('PRAGMA journal_mode=WAL')
    db.execute('PRAGMA synchronous=FULL')
    db.execute('PRAGMA recursive_triggers=ON')
    for table in ('a', 'b', 'c'):
        db.execute(f"CREATE TABLE {table}(id INTEGER PRIMARY KEY,k {fixture.get('table_schema', {'k_sql': 'INTEGER NOT NULL'})['k_sql']},v {fixture.get('table_schema', {'v_sql': 'INTEGER NOT NULL'})['v_sql']})")
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
                'sqlite_ivm: one inner join required',
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
        output = normalized(db.execute(query), domain)
        query_ms = (time.perf_counter()-start)*1000
        inputs = {t: normalized(db.execute(f'SELECT id,k,v FROM {t}'), domain) for t in ('a','b','c')}
        assert inputs == {t: normalized(rows, domain) for t, rows in state['inputs'].items()}, state['name'] + ': inputs'
        assert output == state['expected']['rows'], state['name'] + ': output'
        oracle = normalized(db.execute(fixture['query']), domain)
        assert output == oracle, state['name'] + ': SQL recompute'
        input_hash = digest(''.join(canonical(inputs[t], t.upper(), domain) for t in ('a','b','c')))
        checksum = digest(canonical(output, 'S', domain))
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
             output_bytes=len(canonical(output,'S',domain).encode()), update_transaction_ms=update_ms,
             query_compute_ms=query_ms, update_plus_query_ms=update_ms+query_ms,
             state_inventory=sqlite_inventory(db, args.db, 'circuit_view' if args.extension else None))
    emit(event='case-total', status='ok', update_plus_query_ms=total, final_input_hash=input_hash,
         final_checksum=checksum, disk={'database_bytes':os.path.getsize(args.db),
         'wal_bytes':os.path.getsize(args.db+'-wal') if os.path.exists(args.db+'-wal') else 0})
    db.close()


if __name__ == '__main__':
    main()
