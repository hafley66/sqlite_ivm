"""Untimed, additive maintained-state inventory for SQLite benchmark receipts."""
import os


def metric(value, unit, reason=None):
    return {'value': value, 'unit': unit, 'unavailable_reason': reason}


def _quote(name):
    return '"' + name.replace('"', '""') + '"'


def sqlite_inventory(db, db_path, view_name=None):
    schema = list(db.execute("SELECT type,name,tbl_name,rootpage FROM sqlite_schema WHERE type IN ('table','index') ORDER BY type,name"))
    owned = set()
    if view_name and any(row[1] == '__ivm_objects' for row in schema):
        owned = {(kind, name) for kind, name in db.execute(
            'SELECT object_type,object_name FROM __ivm_objects WHERE view_name=? AND object_type IN (\'table\',\'index\')', (view_name,))}
    owned_tables = {name for kind, name in owned if kind == 'table'}
    source = {'a', 'b', 'c'}
    catalogs = {'__ivm_schema', '__ivm_views', '__ivm_sources', '__ivm_columns', '__ivm_objects'}
    allocation = {}
    dbstat_reason = None
    try:
        for name, allocated, payload in db.execute('SELECT name,sum(pgsize),sum(payload) FROM dbstat GROUP BY name'):
            allocation[name] = (int(allocated), int(payload))
    except Exception as error:
        dbstat_reason = 'SQLite dbstat unavailable: ' + str(error)
    relations = []
    for kind, name, table_name, rootpage in schema:
        if kind == 'table' and name.startswith('sqlite_'):
            continue
        if kind == 'table':
            if name in source:
                role = 'source'
            elif ('table', name) in owned:
                role = 'result' if name.endswith('_result') or name.endswith('_state') else 'support'
            elif name in catalogs:
                role = 'catalog'
            else:
                continue
            count = db.execute('SELECT count(*) FROM main.' + _quote(name)).fetchone()[0]
        else:
            if table_name in source:
                role = 'source-index'
            elif ('index', name) in owned or table_name in owned_tables:
                role = 'support-index'
            elif table_name in catalogs:
                role = 'catalog-index'
            else:
                continue
            count = None
        allocated, payload = allocation.get(name, (None, None))
        missing = dbstat_reason or ('relation has no dbstat pages' if rootpage else 'relation has no physical root page')
        columns = {row[1] for row in db.execute('PRAGMA main.table_info(' + _quote(name) + ')')} if kind == 'table' else set()
        logical_weight = db.execute('SELECT coalesce(sum(__n),0) FROM main.' + _quote(name)).fetchone()[0] if '__n' in columns else None
        relations.append({
            'name': name, 'kind': kind, 'role': role, 'counted_in_totals': True,
            'row_count': metric(int(count) if count is not None else None, 'rows', None if count is not None else 'row count does not apply to an index'),
            'logical_weight_sum': metric(int(logical_weight) if logical_weight is not None else None, 'weighted rows', None if logical_weight is not None else 'relation has no __n multiplicity column'),
            'bytes': {
                'allocated': metric(allocated, 'bytes', None if allocated is not None else missing),
                'data': metric(payload, 'bytes', None if payload is not None else missing),
                'index': metric(allocated if kind == 'index' else None, 'bytes', None if kind == 'index' and allocated is not None else ('index allocation is reported on separate index relations' if kind == 'table' else missing)),
            },
        })
    tables = [r for r in relations if r['kind'] == 'table' and r['counted_in_totals']]
    indexes = [r for r in relations if r['kind'] == 'index' and r['counted_in_totals']]
    def total_metric(items, path, unit):
        values = [path(item)['value'] for item in items]
        if any(value is None for value in values):
            return {**metric(None, unit, 'one or more included relations are unavailable'), 'partial': True,
                    'known_value': sum(value for value in values if value is not None)}
        return {**metric(sum(values), unit), 'partial': False}
    rows_by_role = {}
    for role in sorted({r['role'] for r in tables}):
        rows_by_role[role] = total_metric([r for r in tables if r['role'] == role], lambda r: r['row_count'], 'rows')
    table_bytes = total_metric(tables, lambda r: r['bytes']['allocated'], 'bytes')
    index_bytes = total_metric(indexes, lambda r: r['bytes']['allocated'], 'bytes')
    total_relation = metric(None, 'bytes', 'table or index allocation unavailable')
    if table_bytes['value'] is not None and index_bytes['value'] is not None:
        total_relation = {**metric(table_bytes['value'] + index_bytes['value'], 'bytes'), 'partial': False}
    return {
        'schema_version': 1, 'measured_at': 'after-output-validation', 'outside_timed_region': True,
        'scope': 'SQLite source relations and indexes plus sqlite_ivm-owned result/support/catalog relations; unrelated relations excluded',
        'relations': relations,
        'summary': {
            'table_count': metric(len(tables), 'tables'), 'index_count': metric(len(indexes), 'indexes'),
            'native_collection_count': metric(None, 'collections', 'SQL adapter has no native collection inventory'),
            'total_rows': total_metric(tables, lambda r: r['row_count'], 'rows'), 'rows_by_role': rows_by_role,
            'table_bytes': table_bytes, 'index_bytes': index_bytes, 'total_relation_bytes': total_relation,
        },
        'storage': {
            'database_file_bytes': metric(os.path.getsize(db_path), 'bytes'),
            'wal_file_bytes': metric(os.path.getsize(db_path + '-wal') if os.path.exists(db_path + '-wal') else 0, 'bytes'),
            'database_allocated_bytes': metric(db.execute('PRAGMA page_count').fetchone()[0] * db.execute('PRAGMA page_size').fetchone()[0], 'bytes'),
            'database_size_scope': 'SQLite main database page allocation; database and WAL filesystem lengths are separate',
        },
        'process_memory': {'rss_bytes': metric(None, 'bytes', 'measured by the parent runner as process peak RSS, outside this snapshot')},
        'limitations': ([] if view_name else ['plain query has no durable result relation; output_bytes is transient serialized query output']),
    }
