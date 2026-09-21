#!/usr/bin/env bash
# A crop price belongs to a farm: both key columns must match.
set -euo pipefail
ivm_extension=${1:?Pass the native SQLite extension path}
[[ -f "$ivm_extension" ]] || { printf 'Extension file missing: %s\n' "$ivm_extension" >&2; exit 1; }
ivm_extension_dot=${ivm_extension//\\/\\\\}
ivm_extension_dot=${ivm_extension_dot//\"/\\\"}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-composite.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT
ivm_sqlite=${SQLITE3:-sqlite3}

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TABLE lots(id INTEGER PRIMARY KEY, farm_id INTEGER NOT NULL, crop_id INTEGER NOT NULL, farmer_id INTEGER NOT NULL, crates INTEGER NOT NULL);
CREATE TABLE prices(id INTEGER PRIMARY KEY, farm_id INTEGER NOT NULL, crop_id INTEGER NOT NULL, dollars INTEGER NOT NULL);
INSERT INTO lots VALUES(1,1,10,101,6),(2,2,10,102,8),(3,1,20,101,10);
INSERT INTO prices VALUES(1,1,10,25),(2,2,10,30),(3,1,20,8);
SELECT sqlite_ivm_create('earnings', '
    SELECT l.farmer_id AS farmer,COUNT(*) AS matches,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON (l.farm_id=p.farm_id AND p.crop_id=l.crop_id)
    WHERE l.crates>=5 GROUP BY l.farmer_id
');
SQL

# All mutations run in a fresh process with the virtual-table module loaded.
"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
.headers on
.mode box
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TEMP TABLE checks(step TEXT, ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO checks SELECT 'module loaded',EXISTS(
    SELECT 1 FROM pragma_module_list WHERE name='sqlite_ivm');
CREATE TEMP TABLE expected(farmer INTEGER PRIMARY KEY,matches INTEGER NOT NULL,dollars INTEGER NOT NULL);
CREATE TEMP VIEW recomputed AS
    SELECT l.farmer_id AS farmer,COUNT(*) AS matches,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm_id=p.farm_id AND l.crop_id=p.crop_id
    WHERE l.crates>=5 GROUP BY l.farmer_id;
CREATE TEMP VIEW verified AS SELECT
    NOT EXISTS(SELECT * FROM earnings EXCEPT SELECT * FROM expected)
    AND NOT EXISTS(SELECT * FROM expected EXCEPT SELECT * FROM earnings)
    AND NOT EXISTS(SELECT * FROM earnings EXCEPT SELECT * FROM recomputed)
    AND NOT EXISTS(SELECT * FROM recomputed EXCEPT SELECT * FROM earnings)
    AND (SELECT COUNT(*) FROM earnings)=(SELECT COUNT(*) FROM expected)
    AND (SELECT COUNT(*) FROM earnings)=(SELECT COUNT(*) FROM recomputed)
    AND (SELECT COUNT(*) FROM earnings_delta)=0 AS ok;

.print 'BOOTSTRAP: same crop at different farms gets its own price'
INSERT INTO expected VALUES(101,2,230),(102,1,240);
INSERT INTO checks SELECT 'bootstrap',ok FROM verified;
SELECT * FROM earnings ORDER BY farmer;

INSERT INTO prices VALUES(4,1,10,5);
UPDATE expected SET matches=3,dollars=260 WHERE farmer=101;
INSERT INTO checks SELECT 'duplicate full key',ok FROM verified;

UPDATE prices SET dollars=30 WHERE id=1;
UPDATE expected SET dollars=290 WHERE farmer=101;
INSERT INTO checks SELECT 'price update isolates farm 2',ok FROM verified;

UPDATE lots SET farm_id=2,farmer_id=102,crates=7 WHERE id=1;
UPDATE expected SET matches=1,dollars=80 WHERE farmer=101;
UPDATE expected SET matches=2,dollars=450 WHERE farmer=102;
INSERT INTO checks SELECT 'first key component move',ok FROM verified;

UPDATE prices SET crop_id=20 WHERE id=2;
DELETE FROM expected WHERE farmer=102;
INSERT INTO checks SELECT 'right second key component move',ok FROM verified;

UPDATE lots SET crop_id=20 WHERE id=1;
INSERT INTO expected VALUES(102,1,210);
INSERT INTO checks SELECT 'left second key component move',ok FROM verified;

DELETE FROM prices WHERE id=2;
DELETE FROM expected WHERE farmer=102;
INSERT INTO checks SELECT 'price delete',ok FROM verified;

UPDATE lots SET farm_id=1,farmer_id=101 WHERE id=2;
UPDATE expected SET matches=3,dollars=360 WHERE farmer=101;
INSERT INTO checks SELECT 'lot move with duplicate matches',ok FROM verified;

UPDATE lots SET crates=3 WHERE id=2;
UPDATE expected SET matches=1,dollars=80 WHERE farmer=101;
INSERT INTO checks SELECT 'filter exit',ok FROM verified;
UPDATE lots SET crates=8 WHERE id=2;
UPDATE expected SET matches=3,dollars=360 WHERE farmer=101;
INSERT INTO checks SELECT 'filter entry',ok FROM verified;

BEGIN;
UPDATE lots SET farm_id=99;
UPDATE prices SET farm_id=98;
DELETE FROM expected;
INSERT INTO checks SELECT 'inside transaction',ok FROM verified;
ROLLBACK;
INSERT INTO checks SELECT 'rollback restores both sources',ok FROM verified;

UPDATE prices SET dollars=dollars+1 WHERE farm_id=1 AND crop_id=10;
UPDATE expected SET dollars=376 WHERE farmer=101;
INSERT INTO checks SELECT 'multirow price update',ok FROM verified;
SELECT * FROM earnings ORDER BY farmer;

DELETE FROM lots WHERE id=2;
UPDATE expected SET matches=1,dollars=80 WHERE farmer=101;
INSERT INTO checks SELECT 'lot delete',ok FROM verified;
DELETE FROM lots;
DELETE FROM expected;
INSERT INTO checks SELECT 'delete all',ok FROM verified;
SELECT step,ok FROM checks;
.print 'PASS: composite join CRUD, duplicate keys, filters and rollback in a fresh writer'
SQL
