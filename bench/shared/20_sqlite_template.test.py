"""Exact-state tests for the lab's compiler-SQL trigger transport."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import random
import sqlite3
import tempfile
import unittest

LAB = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('adapter', LAB / '19_sqlite_template_adapter.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class TemplateReuse(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.program = adapter.load_program(os.environ['SQLITE_TEMPLATE_PROGRAM'])
        cls.fixture = json.loads(Path(os.environ['SQLITE_TEMPLATE_FIXTURE']).read_text())

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sqlite-template-reuse-')
        self.path = Path(self.temp.name) / 'test.sqlite'
        self.db = sqlite3.connect(self.path, isolation_level=None)
        adapter.install(self.db, self.program, self.fixture['states'][0]['inputs'])
        self.tables = {rel['rel']: adapter.quoted(rel['table_name']) for rel in self.program['relations']}

    def tearDown(self):
        self.db.close()
        self.temp.cleanup()

    def exact(self):
        actual = adapter.read_state(self.db, self.program)
        expected = {}
        dimensions = dict(actual['dimension'])
        for _, group, amount in actual['fact']:
            if group in dimensions:
                n, total = expected.get(group, (0, 0))
                expected[group] = (n + 1, total + amount * dimensions[group])
        self.assertEqual(actual['summary'], sorted((g, *v) for g, v in expected.items()))
        for rel in self.program['relations']:
            self.assertEqual(self.db.execute('SELECT count(*) FROM ' + adapter.quoted(rel['delta_table_name'])).fetchone(), (0,))
        return actual

    def test_original_crossover_exact_states(self):
        for state in self.fixture['states']:
            if state['name'] != 'initial':
                self.db.execute(state['mutation_sql'])
            adapter.verify(self.db, self.program, state)

    def test_rollback_and_constraint_abort_leave_no_maintenance(self):
        before = self.exact()
        self.db.execute('BEGIN')
        self.db.execute(f'UPDATE {self.tables["fact"]} SET amount=amount+11 WHERE id<12')
        self.exact()
        self.db.execute('SAVEPOINT nested')
        self.db.execute(f'DELETE FROM {self.tables["dimension"]} WHERE group_id=0')
        self.exact()
        self.db.execute('ROLLBACK TO nested')
        self.exact()
        self.db.execute('ROLLBACK')
        self.assertEqual(self.exact(), before)
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute(f'INSERT INTO {self.tables["fact"]}(id,group_id,amount) VALUES(999,1,7),(1,1,8)')
        self.assertEqual(self.exact(), before)

    def test_second_writer_reopen_and_fail_closed_setup(self):
        self.db.close()
        self.db = sqlite3.connect(self.path, isolation_level=None)
        before = self.exact()
        self.db.execute('PRAGMA recursive_triggers=OFF')
        with self.assertRaisesRegex(sqlite3.IntegrityError, 'recursive_triggers required'):
            self.db.execute(f'UPDATE {self.tables["fact"]} SET amount=1 WHERE id=1')
        self.assertEqual(self.exact(), before)
        self.db.execute('PRAGMA trusted_schema=OFF')
        with self.assertRaisesRegex(sqlite3.OperationalError, 'unsafe use of virtual table'):
            self.db.execute(f'UPDATE {self.tables["fact"]} SET amount=1 WHERE id=1')
        self.assertEqual(self.exact(), before)
        self.db.execute('PRAGMA trusted_schema=ON')
        self.db.execute('PRAGMA recursive_triggers=ON')
        other = sqlite3.connect(self.path, isolation_level=None)
        try:
            other.execute('PRAGMA recursive_triggers=ON')
            other.execute(f'UPDATE {self.tables["fact"]} SET amount=1 WHERE id=1')
            self.exact()
        finally:
            other.close()

    def test_replace_upsert_ignore_and_last_group_removal(self):
        operations = [
            f'INSERT OR REPLACE INTO {self.tables["fact"]}(id,group_id,amount) VALUES(1,1,71)',
            f'INSERT INTO {self.tables["fact"]}(id,group_id,amount) VALUES(1,2,19) ON CONFLICT(id) DO UPDATE SET group_id=excluded.group_id,amount=excluded.amount',
            f'INSERT OR IGNORE INTO {self.tables["fact"]}(id,group_id,amount) VALUES(1,3,999)',
            f'DELETE FROM {self.tables["dimension"]} WHERE group_id=0',
            f'INSERT INTO {self.tables["dimension"]}(group_id,factor) VALUES(0,9)',
            f'DELETE FROM {self.tables["fact"]} WHERE group_id=0',
        ]
        for sql in operations:
            self.db.execute(sql)
            self.exact()

    def test_seeded_ordinary_write_sequences(self):
        steps = 0
        initial = self.exact()
        expected_facts = {row[0]: row for row in initial['fact']}
        expected_dimensions = dict(initial['dimension'])
        for seed in [7, 42, 2026]:
            rng = random.Random(seed)
            for _ in range(50):
                row_id, group, amount = rng.randrange(1, 450), rng.randrange(20), rng.randrange(-500, 501)
                operation = rng.randrange(4)
                if operation == 0:
                    self.db.execute(f'INSERT OR REPLACE INTO {self.tables["fact"]}(id,group_id,amount) VALUES(?,?,?)', (row_id, group, amount))
                    expected_facts[row_id] = (row_id, group, amount)
                elif operation == 1:
                    self.db.execute(f'DELETE FROM {self.tables["fact"]} WHERE id=?', (row_id,))
                    expected_facts.pop(row_id, None)
                elif operation == 2:
                    self.db.execute(f'UPDATE {self.tables["fact"]} SET group_id=?,amount=? WHERE id=?', (group, amount, row_id))
                    if row_id in expected_facts:
                        expected_facts[row_id] = (row_id, group, amount)
                else:
                    factor = rng.randrange(-5, 6)
                    self.db.execute(f'UPDATE {self.tables["dimension"]} SET factor=? WHERE group_id=?', (factor, group))
                    expected_dimensions[group] = factor
                actual = self.exact()
                self.assertEqual(actual['fact'], sorted(expected_facts.values()))
                self.assertEqual(actual['dimension'], sorted(expected_dimensions.items()))
                steps += 1
        self.assertEqual(steps, 150)

    def test_unsupported_plan_and_null_source_rejected(self):
        for field, value in [('edges', [{}]), ('uses_tick', True), ('intern_mode', 'dict')]:
            unsupported = copy.deepcopy(self.program)
            unsupported[field] = value
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                adapter.trigger_sql(unsupported)
        unsupported = copy.deepcopy(self.program)
        next(rel for rel in unsupported['relations'] if rel['rel'] == 'fact')['key_indices'] = []
        with self.assertRaisesRegex(ValueError, 'unsupported'):
            adapter.trigger_sql(unsupported)
        before = self.exact()
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute(f'INSERT INTO {self.tables["fact"]}(id,group_id,amount) VALUES(999,1,NULL)')
        self.assertEqual(self.exact(), before)


if __name__ == '__main__':
    print('SQLite ' + sqlite3.sqlite_version + '; exact emitted-SQL transport tests; no public SQL installer', flush=True)
    unittest.main()
