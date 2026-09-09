#!/usr/bin/env bash
# Stock SQLite loads the extension to install; a fresh ordinary writer then reopens it.
set -euo pipefail
ivm_extension=${1:?Pass the native SQLite extension path}
[[ -f "$ivm_extension" ]] || { printf 'Extension file missing: %s\n' "$ivm_extension" >&2; exit 1; }
ivm_extension_dot=${ivm_extension//\\/\\\\}
ivm_extension_dot=${ivm_extension_dot//\"/\\\"}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-join.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT
ivm_sqlite=${SQLITE3:-sqlite3}

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
.headers on
.mode box
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TABLE items(id INTEGER PRIMARY KEY, join_key INTEGER NOT NULL, group_id INTEGER NOT NULL, amount INTEGER NOT NULL);
CREATE TABLE dimensions(id INTEGER PRIMARY KEY, join_key INTEGER NOT NULL, factor INTEGER NOT NULL);
SELECT sqlite_ivm_create('totals', '
    SELECT i.group_id AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i JOIN dimensions d ON i.join_key=d.join_key
    GROUP BY i.group_id
');
SELECT sqlite_ivm_create('positive', '
    SELECT i.group_id AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i JOIN dimensions d ON i.join_key=d.join_key
    WHERE i.amount>0 AND d.factor<>0
    GROUP BY i.group_id
');
CREATE TEMP TABLE expected(g INTEGER PRIMARY KEY, n INTEGER NOT NULL, s INTEGER NOT NULL);
CREATE TEMP VIEW recomputed AS
    SELECT i.group_id AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i JOIN dimensions d ON i.join_key=d.join_key GROUP BY i.group_id;
CREATE TEMP VIEW recomputed_positive AS
    SELECT i.group_id AS g, COUNT(*) AS n, SUM(i.amount*d.factor) AS s
    FROM items i JOIN dimensions d ON i.join_key=d.join_key
    WHERE i.amount>0 AND d.factor<>0 GROUP BY i.group_id;
CREATE TEMP VIEW verified AS SELECT
    NOT EXISTS(SELECT * FROM totals EXCEPT SELECT * FROM expected)
    AND NOT EXISTS(SELECT * FROM expected EXCEPT SELECT * FROM totals)
    AND NOT EXISTS(SELECT * FROM totals EXCEPT SELECT * FROM recomputed)
    AND NOT EXISTS(SELECT * FROM recomputed EXCEPT SELECT * FROM totals)
    AND (SELECT COUNT(*) FROM totals)=(SELECT COUNT(*) FROM expected)
    AND NOT EXISTS(SELECT * FROM positive EXCEPT SELECT * FROM recomputed_positive)
    AND NOT EXISTS(SELECT * FROM recomputed_positive EXCEPT SELECT * FROM positive)
    AND (SELECT COUNT(*) FROM positive)=(SELECT COUNT(*) FROM recomputed_positive) AS ok;
CREATE TEMP TABLE checks(step TEXT, ok INTEGER NOT NULL CHECK(ok=1));

.print 'INSERT ITEMS WITHOUT MATCHES: expect no result rows'
INSERT INTO items VALUES (1,10,4,7),(2,10,4,3),(3,20,9,-2);
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'unmatched items',ok FROM verified;

.print 'INSERT DIMENSIONS WITH DUPLICATE KEYS: expect (4,4,10), (9,1,-8)'
INSERT INTO dimensions VALUES (1,10,2),(2,10,-1),(3,20,4);
INSERT INTO expected VALUES (4,4,10),(9,1,-8);
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'dimension inserts',ok FROM verified;

.print 'INSERT MATCHING ITEM: expect group 4 = (4,6,15)'
INSERT INTO items VALUES (5,10,4,5);
UPDATE expected SET n=6,s=15 WHERE g=4;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'matching item insert',ok FROM verified;

.print 'UPDATE ITEM KEY, GROUP AND AMOUNT: expect (4,4,8), (9,2,36)'
UPDATE items SET join_key=20,group_id=9,amount=11 WHERE id=1;
UPDATE expected SET n=4,s=8 WHERE g=4;
UPDATE expected SET n=2,s=36 WHERE g=9;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'item update',ok FROM verified;

.print 'UPDATE DIMENSION JOIN KEY: expect (4,2,16), (9,4,27)'
UPDATE dimensions SET join_key=20 WHERE id=2;
UPDATE expected SET n=2,s=16 WHERE g=4;
UPDATE expected SET n=4,s=27 WHERE g=9;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'dimension update',ok FROM verified;

.print 'DELETE NEGATIVE ITEM: expect group 9 = (9,2,33)'
DELETE FROM items WHERE id=3;
UPDATE expected SET n=2,s=33 WHERE g=9;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'item delete',ok FROM verified;

.print 'DELETE DIMENSIONS INSIDE TRANSACTION, THEN ROLLBACK'
BEGIN;
DELETE FROM dimensions;
DELETE FROM expected;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'transactional delete',ok FROM verified;
ROLLBACK;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'rollback',ok FROM verified;

.print 'DELETE ALL MATCHES FOR GROUP 9: expect only (4,2,16)'
DELETE FROM dimensions WHERE join_key=20;
DELETE FROM expected WHERE g=9;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT 'dimension delete',ok FROM verified;

.print 'ITEM LEAVES FILTER: positive result is (4,1,10)'
UPDATE items SET amount=-3 WHERE id=2;
UPDATE expected SET s=4 WHERE g=4;
SELECT * FROM positive ORDER BY g;
INSERT INTO checks SELECT 'item leaves filter',ok FROM verified;

.print 'ITEM ENTERS FILTER: positive result is (4,2,16)'
UPDATE items SET amount=3 WHERE id=2;
UPDATE expected SET s=16 WHERE g=4;
SELECT * FROM positive ORDER BY g;
INSERT INTO checks SELECT 'item enters filter',ok FROM verified;

.print 'DIMENSION LEAVES FILTER: positive result is empty'
UPDATE dimensions SET factor=0 WHERE id=1;
UPDATE expected SET s=0 WHERE g=4;
SELECT * FROM positive ORDER BY g;
INSERT INTO checks SELECT 'dimension leaves filter',ok FROM verified;

.print 'DIMENSION ENTERS FILTER: positive result is (4,2,16)'
UPDATE dimensions SET factor=2 WHERE id=1;
UPDATE expected SET s=16 WHERE g=4;
SELECT * FROM positive ORDER BY g;
INSERT INTO checks SELECT 'dimension enters filter',ok FROM verified;
SELECT step,ok FROM checks;
SQL

# A fresh process reconnects the persisted virtual tables through the module.
"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
.headers on
.mode box
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TEMP TABLE checks(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO checks SELECT EXISTS(
    SELECT 1 FROM pragma_module_list WHERE name='sqlite_ivm');
.print 'FRESH WRITER, MODULE LOADED: factor update gives (4,2,24)'
UPDATE dimensions SET factor=3 WHERE join_key=10;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT COUNT(*)=1 AND MIN(g)=4 AND MIN(n)=2 AND MIN(s)=24 FROM totals;
INSERT INTO checks SELECT COUNT(*)=1 AND MIN(g)=4 AND MIN(n)=2 AND MIN(s)=24 FROM positive;
.print 'FRESH WRITER FILTER EXIT: positive result is empty'
UPDATE items SET amount=-amount;
SELECT * FROM positive ORDER BY g;
INSERT INTO checks SELECT COUNT(*)=0 FROM positive;
INSERT INTO checks SELECT COUNT(*)=1 AND MIN(g)=4 AND MIN(n)=2 AND MIN(s)=-24 FROM totals;
.print 'DELETE ALL ITEMS: expect no result rows'
DELETE FROM items;
SELECT * FROM totals ORDER BY g;
INSERT INTO checks SELECT COUNT(*)=0 FROM totals;
INSERT INTO checks SELECT COUNT(*)=0 FROM positive;
.print 'PASS: native join CRUD, filters, rollback and fresh-writer assertions passed'
SQL
