"""Lab transport of compiler-emitted terminal aggregate SQL into SQLite triggers.

No SELECT parser, extension API, or new aggregate algorithm. COUNT/SUM use the
existing compiler's affected-group recomputation, once per changed source row.
"""

import argparse
import hashlib
import json
import re
import resource
import sqlite3
import sys
import time
from pathlib import Path


def quoted(name):
    return '"' + name.replace('"', '""') + '"'


def load_program(path):
    source = Path(path).read_text()
    match = re.search(r'pub const PROGRAM_JSON: &str = r(#+)"\n(.*?)\n"\1;', source, re.S)
    if match is None:
        raise ValueError('expected existing emit_rust PROGRAM_JSON artifact')
    return json.loads(match[2])


def trigger_sql(program):
    if (program['edges'] or program['host_plans'] or program['uses_tick']
            or program['intern_mode'] != 'none' or len(program['levels']) != 1):
        raise ValueError('unsupported: terminal single-level aggregate without hosts/edges/interning only')
    level = program['levels'][0]
    aggregate = level['aggregate_sql']
    if (aggregate is None or aggregate['delta_maintained'] or aggregate.get('intern_sql')
            or level['recursion_group'] is not None or program['queries'] != [level['head_rel']]):
        raise ValueError('unsupported: terminal affected-group aggregate only')
    sources = [rel for rel in program['relations'] if rel['rel'] in program['arrival_targets']]
    if any(rel['kind'] != 'set' or not rel['key_indices']
           or any(t != 'int' for t in rel['column_types']) for rel in sources):
        raise ValueError('unsupported: integer keyed set sources only')
    returning = ' RETURNING ' + ', '.join(map(quoted, level['head_columns']))

    def terminal_sql(sql):
        if not sql.endswith(returning):
            raise ValueError('unsupported generated RETURNING shape')
        # The terminal result has no downstream event consumer. Core query text
        # is preserved byte-for-byte; SQLite triggers prohibit RETURNING.
        return sql[:-len(returning)]

    maintenance = [aggregate['scope_clear_sql'], *aggregate['scope_seed_sql'],
                   terminal_sql(aggregate['delete_scoped_sql']),
                   *map(terminal_sql, aggregate['insert_scoped_sql'])]
    clear = ['DELETE FROM ' + quoted(rel['delta_table_name']) for rel in sources]
    statements = []
    for rel in sources:
        for event, images in [('INSERT', [('NEW', 1)]), ('DELETE', [('OLD', -1)]),
                              ('UPDATE', [('OLD', -1), ('NEW', 1)])]:
            name = '__reuse_' + rel['rel'] + '_' + event.lower()
            guard = "SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)=0 THEN RAISE(ABORT, 'recursive_triggers required') END"
            # Fail closed before writing when a connection lacks required settings.
            statements.append(f'CREATE TRIGGER {quoted(name + "_guard")} BEFORE {event} ON {quoted(rel["table_name"])} BEGIN {guard}; END')
            stage = []
            cols = ', '.join(map(quoted, ['_sign', '_sequence', *rel['columns']]))
            for sequence, (image, sign) in enumerate(images):
                values = ', '.join([str(sign), str(sequence), *[image + '.' + quoted(c) for c in rel['columns']]])
                stage.append(f'INSERT INTO {quoted(rel["delta_table_name"])} ({cols}) VALUES ({values})')
            body = [*clear, *stage, *maintenance, *clear, aggregate['scope_clear_sql']]
            statements.append(f'CREATE TRIGGER {quoted(name)} AFTER {event} ON {quoted(rel["table_name"])} BEGIN\n' + ';\n'.join(body) + ';\nEND')
    return statements


def install(db, program, initial, sql_path=None):
    db.execute('PRAGMA trusted_schema=ON')
    db.execute('PRAGMA recursive_triggers=ON')
    db.execute('PRAGMA journal_mode=WAL')
    db.execute('PRAGMA synchronous=FULL')
    triggers = trigger_sql(program)  # Validate before creating objects.
    ddl = [sql.replace('CREATE TEMP TABLE ', 'CREATE TABLE ', 1) if sql.startswith('CREATE TEMP TABLE ') else sql for sql in program['ddl']]
    db.execute('SAVEPOINT reuse_install')
    try:
        for sql in ddl:
            db.execute(sql)
        for rel, rows in initial.items():
            db.executemany(program['arrival_templates'][rel]['add_sql'], rows)
        for statement in program['boot']:
            db.execute(statement['sql'], statement['params']).fetchall()
        # Match the existing crossover fixture's source group index.
        fact = next(rel for rel in program['relations'] if rel['rel'] == 'fact')
        db.execute(f'CREATE INDEX fact_group_idx ON {quoted(fact["table_name"])}(group_id)')
        for sql in triggers:
            db.execute(sql)
        db.execute('RELEASE reuse_install')
    except Exception:
        db.execute('ROLLBACK TO reuse_install')
        db.execute('RELEASE reuse_install')
        raise
    if sql_path:
        Path(sql_path).write_text('-- Generated lab transport. No SQL parser; affected-group recomputation.\n' + ';\n'.join([*ddl, *triggers]) + ';\n')


def read_state(db, program):
    return {name: sorted(db.execute(sql).fetchall()) for name, sql in program['final_select'].items()}


def verify(db, program, fixture_state):
    actual = read_state(db, program)
    for rel, expected in fixture_state['inputs'].items():
        assert actual[rel] == list(map(tuple, expected)), (rel, actual[rel], expected)
    # Independent recomputation from source rows. No emitted maintenance SQL.
    dimensions = dict(actual['dimension'])
    summary = {}
    for _, group, amount in actual['fact']:
        if group in dimensions:
            count, total = summary.get(group, (0, 0))
            summary[group] = (count + 1, total + amount * dimensions[group])
    expected = sorted((group, *value) for group, value in summary.items())
    assert actual['summary'] == expected, (actual['summary'], expected)
    canonical = '\n'.join('S\t' + '\t'.join(map(str, row)) for row in expected)
    checksum = hashlib.sha256(canonical.encode()).hexdigest()
    assert checksum == fixture_state['expected']['checksum']
    return actual, checksum, len(canonical.encode())


def main(plugin_install=None):
    parser = argparse.ArgumentParser()
    parser.add_argument('--program', required=plugin_install is None)
    parser.add_argument('--extension')
    parser.add_argument('--metrics', type=int, default=0)
    parser.add_argument('--fixture', required=True)
    parser.add_argument('--db', required=True)
    parser.add_argument('--sql-output')
    args = parser.parse_args()
    program = load_program(args.program) if plugin_install is None else {
        "final_select": {"fact":"SELECT id,group_id,amount FROM fact", "dimension":"SELECT group_id,factor FROM dimension", "summary":"SELECT group_id,n,s FROM summary"}}
    fixture = json.loads(Path(args.fixture).read_text())
    if Path(args.db).exists():
        raise ValueError('refusing to overwrite prior SQLite receipt')
    db = sqlite3.connect(args.db, isolation_level=None)
    started = time.perf_counter()
    provenance = {}
    if plugin_install is None:
        install(db, program, fixture['states'][0]['inputs'], args.sql_output)
    else:
        provenance = plugin_install(db, fixture['states'][0]['inputs'], args.extension, args.metrics, args.sql_output)
    setup_ms = (time.perf_counter() - started) * 1000
    print(json.dumps({'event': 'case-setup', 'status': 'ok', 'setup_ms': setup_ms,
                      'program_sha256': hashlib.sha256(Path(args.program).read_bytes()).hexdigest() if args.program else None,
                      'runtime': 'stock SQLite ' + sqlite3.sqlite_version,
                      'algorithm': 'compiler-emitted affected-group recomputation in persistent row triggers',
                      'sql_consumer_install_api': plugin_install is not None, 'durability': 'WAL/synchronous=FULL',
                      'memory_scope': 'Python process incl SQLite; no enforced total cap', **provenance}), flush=True)
    total = 0
    previous_summary = set()
    for state in fixture['states']:
        started = time.perf_counter()
        if state['name'] != 'initial':
            db.execute('BEGIN IMMEDIATE')
            try:
                db.execute(state['mutation_sql'])
                affected = db.execute('SELECT changes()').fetchone()[0]
                db.execute('COMMIT')
            except Exception as error:
                db.execute('ROLLBACK')
                if plugin_install is not None:
                    print(json.dumps({'event':'transaction-rollback','state':state['name'],'extended_code':getattr(error,'sqlite_errorcode',None)}),file=sys.stderr)
                raise
        else:
            affected = 0
        update_ms = (time.perf_counter() - started) * 1000 if state['name'] != 'initial' else 0
        started = time.perf_counter()
        db.execute('CREATE TEMP TABLE crossover_snapshot AS ' + program['final_select']['summary'])
        materialized_count = db.execute('SELECT count(*) FROM crossover_snapshot').fetchone()[0]
        compute_ms = (time.perf_counter() - started) * 1000
        actual, checksum, output_bytes = verify(db, program, state)
        current_summary = set(actual['summary'])
        output_insertions = len(current_summary - previous_summary)
        output_retractions = len(previous_summary - current_summary)
        previous_summary = current_summary
        input_text = '\n'.join(['D\t' + '\t'.join(map(str, row)) for row in actual['dimension']]
                               + ['F\t' + '\t'.join(map(str, row)) for row in actual['fact']])
        input_hash = hashlib.sha256(input_text.encode()).hexdigest()
        if 'input_hash' in state:
            assert input_hash == state['input_hash']
        assert materialized_count == len(actual['summary'])
        db.execute('DROP TABLE crossover_snapshot')
        if plugin_install is not None and args.metrics:
            counters = db.execute('SELECT operations,contributions,groups_touched FROM __ivm_73756d6d617279_meta').fetchone()
            print(json.dumps({'event':'observed-transaction','state':state['name'],'boundary':'initial' if state['name']=='initial' else 'commit-returned','affected_inputs':affected,'duration_ms':update_ms,'cumulative_counters':counters,'counter_scope':'transactional operations/contributions/groups touched; OLD and NEW separately'}),file=sys.stderr)
        assert affected == state['expected_affected_rows']
        if state['name'] != 'initial':
            total += update_ms + compute_ms
        print(json.dumps({'event': 'mutation', 'status': 'ok', 'state': state['name'],
                          'affected_rows': affected, 'join_affected_rows': state['join_affected_rows'],
                          'update_transaction_ms': update_ms, 'query_compute_ms': compute_ms,
                          'update_plus_query_ms': update_ms + compute_ms,
                          'checksum': checksum, 'output_rows': len(actual['summary']),
                          'input_hash': input_hash, 'materialized_count': materialized_count,
                          'output_bytes': output_bytes, 'output_insertions': output_insertions,
                          'output_retractions': output_retractions,
                          'output_delta_scope': 'distinct aggregate rows versus previous verified snapshot; outside timer',
                          'exact_input_output_validated': True,
                          'summary': actual['summary']}), flush=True)
    live_disk = {name: Path(args.db + suffix).stat().st_size if Path(args.db + suffix).exists() else 0 for name,suffix in [('main_bytes',''),('wal_bytes','-wal'),('shm_bytes','-shm')]}
    db.close()
    db = sqlite3.connect(args.db, isolation_level=None)
    verify(db, program, fixture['states'][-1])
    db.close()
    print(json.dumps({'event': 'case-total', 'status': 'ok', 'update_plus_query_ms': total,
                      'final_checksum': checksum, 'fresh_reopen_validated': True,
                      'final_input_hash': input_hash,
                      'disk': {'database_bytes': Path(args.db).stat().st_size, **live_disk, 'temp_bytes': None, 'temp_scope':'SQLite temp files not sampled'},
                      'process_peak_rss_platform_units': resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
                      'rss_units': 'bytes' if sys.platform == 'darwin' else 'KiB'}), flush=True)


if __name__ == '__main__':
    main()
