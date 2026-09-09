#!/usr/bin/env bash
# Acceptance script for the native sqlite_ivm_create(name, SELECT) API.
# Usage: bash sqlite_ivm/scripts/1_crud.sh /absolute/path/to/sqlite_ivm.dylib
set -euo pipefail

ivm_extension=${1:?Pass the native SQLite extension path}
if [[ ! -f "$ivm_extension" ]]; then
  printf 'Extension file missing: %s\n' "$ivm_extension" >&2
  exit 1
fi
ivm_extension_sql=${ivm_extension//\'/\'\'}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-crud.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT

"${SQLITE3:-sqlite3}" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.headers on
.mode box
SELECT load_extension('$ivm_extension_sql');
PRAGMA recursive_triggers = ON;
PRAGMA trusted_schema = ON;

CREATE TABLE items (
  id INTEGER PRIMARY KEY,
  group_id INTEGER NOT NULL,
  amount INTEGER NOT NULL
);

SELECT sqlite_ivm_create('totals', '
  SELECT group_id, COUNT(*) AS item_count, SUM(amount) AS total_amount
  FROM items
  GROUP BY group_id
');

CREATE TEMP TABLE expected (
  group_id INTEGER PRIMARY KEY,
  item_count INTEGER NOT NULL,
  total_amount INTEGER NOT NULL
);
CREATE TEMP VIEW verified AS
SELECT
  NOT EXISTS (
    SELECT group_id, item_count, total_amount FROM totals
    EXCEPT SELECT group_id, item_count, total_amount FROM expected
  )
  AND NOT EXISTS (
    SELECT group_id, item_count, total_amount FROM expected
    EXCEPT SELECT group_id, item_count, total_amount FROM totals
  )
  AND (SELECT COUNT(*) FROM totals) = (SELECT COUNT(*) FROM expected)
  AS ok;
CREATE TEMP TABLE checks (
  step TEXT NOT NULL,
  ok INTEGER NOT NULL CHECK (ok = 1)
);

.print 'EMPTY: expect no groups'
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'empty', ok FROM verified;

.print 'INSERT: expect (4,2,10), (9,1,-2), (12,1,0)'
INSERT INTO items (id, group_id, amount) VALUES
  (1, 4, 7), (2, 4, 3), (3, 9, -2), (4, 12, 0);
INSERT INTO expected VALUES (4, 2, 10), (9, 1, -2), (12, 1, 0);
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'insert', ok FROM verified;

.print 'UPDATE AMOUNT: group 4 sum becomes 14'
UPDATE items SET amount = 11 WHERE id = 1;
UPDATE expected SET total_amount = 14 WHERE group_id = 4;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'update amount', ok FROM verified;

.print 'MOVE GROUP + CHANGE AMOUNT: expect (4,1,3), (9,2,5), (12,1,0)'
UPDATE items SET group_id = 9, amount = 7 WHERE id = 1;
UPDATE expected SET item_count = 1, total_amount = 3 WHERE group_id = 4;
UPDATE expected SET item_count = 2, total_amount = 5 WHERE group_id = 9;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'move group and change amount', ok FROM verified;

.print 'MULTIROW UPDATE: group 9 sum becomes 7'
UPDATE items SET amount = amount + 1 WHERE group_id = 9;
UPDATE expected SET total_amount = 7 WHERE group_id = 9;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'multirow update', ok FROM verified;

.print 'DELETE NEGATIVE ROW: group 9 becomes (9,1,8)'
DELETE FROM items WHERE id = 3;
UPDATE expected SET item_count = 1, total_amount = 8 WHERE group_id = 9;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'delete negative row', ok FROM verified;

.print 'DELETE LAST ROW IN GROUP: group 4 disappears'
DELETE FROM items WHERE id = 2;
DELETE FROM expected WHERE group_id = 4;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'delete last row in group', ok FROM verified;

.print 'DELETE INSIDE TRANSACTION: expect no groups'
BEGIN;
DELETE FROM items;
DELETE FROM expected;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'delete inside transaction', ok FROM verified;

.print 'ROLLBACK: expect (9,1,8), (12,1,0)'
ROLLBACK;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'rollback restores groups', ok FROM verified;

.print 'DELETE ALL: expect no groups'
DELETE FROM items;
DELETE FROM expected;
SELECT * FROM totals ORDER BY group_id;
INSERT INTO checks SELECT 'delete all', ok FROM verified;

.print 'PASS: all CRUD and rollback assertions passed'
SELECT step, ok FROM checks;
SQL
