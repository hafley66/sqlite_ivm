"""Stock SQLite mechanism probes, not a query compiler or IVM benchmark.

Run: python3 17_sqlite_trigger_capabilities.py -v
Hand-authored event-log triggers expose transaction and writer-setup boundaries.
No dependencies, extension installation, production changes, or retained DB files.
"""

import sqlite3
import tempfile
import unittest
from pathlib import Path


class TriggerCapabilities(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sqlite-ivm-capabilities-")
        self.path = Path(self.temp.name) / "probe.sqlite"
        self.db = sqlite3.connect(self.path, isolation_level=None)
        self.db.executescript("""
            CREATE TABLE source(id INTEGER PRIMARY KEY, value);
            CREATE TABLE events(op, id, value);
            CREATE TRIGGER source_insert AFTER INSERT ON source BEGIN
              INSERT INTO events VALUES('I', NEW.id, NEW.value);
            END;
            CREATE TRIGGER source_delete AFTER DELETE ON source BEGIN
              INSERT INTO events VALUES('D', OLD.id, OLD.value);
            END;
            CREATE TRIGGER source_update AFTER UPDATE ON source BEGIN
              INSERT INTO events VALUES('D', OLD.id, OLD.value);
              INSERT INTO events VALUES('I', NEW.id, NEW.value);
            END;
        """)

    def tearDown(self):
        self.db.close()
        self.temp.cleanup()

    def state(self, db=None):
        connection = db or self.db
        return (
            connection.execute("SELECT * FROM source ORDER BY id").fetchall(),
            connection.execute("SELECT * FROM events ORDER BY rowid").fetchall(),
        )

    def test_replace_without_recursive_triggers_omits_delete(self):
        self.db.execute("PRAGMA recursive_triggers=OFF")
        self.db.execute("INSERT INTO source VALUES(1, 10)")
        self.db.execute("INSERT OR REPLACE INTO source VALUES(1, 20)")
        self.assertEqual(self.state(), ([(1, 20)], [('I', 1, 10), ('I', 1, 20)]))

    def test_replace_and_upsert_with_recursive_triggers(self):
        self.db.execute("PRAGMA recursive_triggers=ON")
        self.db.execute("INSERT INTO source VALUES(1, NULL)")
        self.db.execute("INSERT OR REPLACE INTO source VALUES(1, 20)")
        self.db.execute("INSERT INTO source VALUES(1, 30) ON CONFLICT(id) DO UPDATE SET value=excluded.value")
        self.assertEqual(self.state(), ([(1, 30)], [
            ('I', 1, None), ('D', 1, None), ('I', 1, 20), ('D', 1, 20), ('I', 1, 30),
        ]))

    def test_transaction_and_savepoint_rollback(self):
        self.db.execute("INSERT INTO source VALUES(1, 10)")
        original = self.state()
        self.db.execute("BEGIN")
        self.db.execute("UPDATE source SET value=20")
        self.db.execute("SAVEPOINT inner_step")
        self.db.execute("DELETE FROM source")
        self.db.execute("ROLLBACK TO inner_step")
        self.assertEqual(self.state(), ([(1, 20)], [('I', 1, 10), ('D', 1, 10), ('I', 1, 20)]))
        self.db.execute("ROLLBACK")
        self.assertEqual(self.state(), original)

    def test_persistent_triggers_second_connection_and_reopen(self):
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("INSERT INTO source VALUES(1, 10)")
        second = sqlite3.connect(self.path, isolation_level=None)
        try:
            second.execute("UPDATE source SET value=20")
            expected = ([(1, 20)], [('I', 1, 10), ('D', 1, 10), ('I', 1, 20)])
            self.assertEqual(self.state(second), expected)
            self.assertEqual(self.state(), expected)
        finally:
            second.close()
        self.db.close()
        self.db = sqlite3.connect(self.path, isolation_level=None)
        self.assertEqual(self.state(), expected)
        self.db.execute("DELETE FROM source")
        self.assertEqual(self.state(), ([], expected[1] + [('D', 1, 20)]))

    def test_guard_fails_closed_for_writer_settings(self):
        self.db.execute("""CREATE TRIGGER writer_guard BEFORE INSERT ON source BEGIN
            SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)=0
              THEN RAISE(ABORT, 'recursive_triggers required') END;
        END""")
        self.db.execute("PRAGMA recursive_triggers=OFF")
        with self.assertRaisesRegex(sqlite3.IntegrityError, 'recursive_triggers required'):
            self.db.execute("INSERT INTO source VALUES(1, 10)")
        self.assertEqual(self.state(), ([], []))
        self.db.execute("PRAGMA recursive_triggers=ON")
        self.db.execute("INSERT INTO source VALUES(1, 10)")
        original = self.state()
        self.db.execute("PRAGMA trusted_schema=OFF")
        with self.assertRaisesRegex(sqlite3.OperationalError, 'unsafe use of virtual table'):
            self.db.execute("INSERT INTO source VALUES(2, 20)")
        self.assertEqual(self.state(), original)

    def test_outer_conflict_policy_overrides_trigger_ignore(self):
        self.db.executescript("""
            CREATE TABLE singleton(key INTEGER PRIMARY KEY, value);
            INSERT INTO singleton VALUES(0, 'original');
            CREATE TRIGGER collision AFTER INSERT ON source BEGIN
              INSERT OR IGNORE INTO singleton VALUES(0, NEW.value);
            END;
        """)
        self.db.execute("INSERT INTO source VALUES(1, 'ordinary')")
        self.assertEqual(self.db.execute("SELECT * FROM singleton").fetchall(), [(0, 'original')])
        self.db.execute("INSERT OR REPLACE INTO source VALUES(2, 'replacement')")
        self.assertEqual(self.db.execute("SELECT * FROM singleton").fetchall(), [(0, 'replacement')])

    def test_statement_abort_rolls_back_trigger_events(self):
        self.db.execute("INSERT INTO source VALUES(1, 10)")
        original = self.state()
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO source VALUES(2, 20), (1, 30)")
        self.assertEqual(self.state(), original)

    def test_scalar_callback_schema_setup_obeys_outer_rollback(self):
        # Public scalar-function callback mechanism only. No SQL parser/IVM.
        def setup_probe():
            self.db.execute("CREATE TABLE installed(value)")
            self.db.execute("INSERT INTO installed VALUES(7)")
            return 1

        self.db.create_function("setup_probe", 0, setup_probe)
        self.db.execute("BEGIN")
        self.assertEqual(self.db.execute("SELECT setup_probe()").fetchall(), [(1,)])
        self.assertEqual(self.db.execute("SELECT * FROM installed").fetchall(), [(7,)])
        self.db.execute("ROLLBACK")
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name='installed'").fetchall(), [])

    def test_scalar_callback_error_requires_explicit_atomic_setup(self):
        def partial_setup():
            self.db.execute("CREATE TABLE partial(value)")
            self.db.execute("INSERT INTO missing_table VALUES(7)")

        self.db.create_function("partial_setup", 0, partial_setup)
        with self.assertRaises(sqlite3.OperationalError):
            self.db.execute("SELECT partial_setup()")
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name='partial'").fetchall(), [('partial',)])

    def test_scalar_callback_savepoint_removes_partial_setup(self):
        def atomic_setup():
            self.db.execute("SAVEPOINT install_probe")
            try:
                self.db.execute("CREATE TABLE partial(value)")
                self.db.execute("INSERT INTO missing_table VALUES(7)")
            except sqlite3.Error:
                self.db.execute("ROLLBACK TO install_probe")
                self.db.execute("RELEASE install_probe")
                raise

        self.db.create_function("atomic_setup", 0, atomic_setup)
        with self.assertRaises(sqlite3.OperationalError):
            self.db.execute("SELECT atomic_setup()")
        self.assertEqual(self.db.execute("SELECT name FROM sqlite_schema WHERE name='partial'").fetchall(), [])


if __name__ == '__main__':
    print(f"SQLite {sqlite3.sqlite_version}; stdlib sqlite3; temporary on-disk DBs; no IVM candidate executed", flush=True)
    unittest.main()
