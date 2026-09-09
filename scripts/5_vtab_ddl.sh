#!/usr/bin/env bash
# Native virtual-table DDL, persisted reconnect, rename rollback and protected state.
set -euo pipefail
ivm_extension=${1:?Pass the native SQLite extension path}
[[ -f "$ivm_extension" ]] || { printf 'Extension file missing: %s\n' "$ivm_extension" >&2; exit 1; }
ivm_extension_dot=${ivm_extension//\\/\\\\}
ivm_extension_dot=${ivm_extension_dot//\"/\\\"}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-vtab.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT
ivm_sqlite=${SQLITE3:-sqlite3}

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
.dbconfig defensive on
CREATE TABLE lots(id INTEGER PRIMARY KEY,farm INTEGER NOT NULL,crop INTEGER NOT NULL,farmer INTEGER NOT NULL,crates INTEGER NOT NULL);
CREATE TABLE prices(id INTEGER PRIMARY KEY,farm INTEGER NOT NULL,crop INTEGER NOT NULL,dollars INTEGER NOT NULL);
INSERT INTO lots VALUES(1,1,10,101,7),(2,2,10,102,6);
INSERT INTO prices VALUES(1,1,10,20),(2,2,10,30);
CREATE VIRTUAL TABLE earnings USING sqlite_ivm('
    SELECT l.farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm=p.farm AND l.crop=p.crop
    WHERE l.crates>=5 GROUP BY l.farmer');
CREATE TEMP TABLE checks(step TEXT,ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO checks SELECT 'initial earnings',COUNT(*)=2 AND SUM(dollars)=320 FROM earnings;
INSERT INTO checks SELECT 'shadow classification',COUNT(*)=2 FROM pragma_table_list
    WHERE name GLOB 'earnings_*' AND type='shadow';
BEGIN;
ALTER TABLE earnings RENAME TO temporary_income;
UPDATE lots SET crates=3 WHERE id=1;
INSERT INTO checks SELECT 'renamed filter exit',COUNT(*)=1 AND MIN(dollars)=180 FROM temporary_income;
ROLLBACK;
INSERT INTO checks SELECT 'rename rollback',COUNT(*)=2 AND SUM(dollars)=320 FROM earnings;
ALTER TABLE earnings RENAME TO income;
INSERT INTO checks SELECT 'renamed persisted state',COUNT(*)=2 AND SUM(dollars)=320 FROM income;
EXPLAIN QUERY PLAN SELECT * FROM income WHERE farmer=101;
SELECT step,ok FROM checks;
SQL

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
.dbconfig defensive on
CREATE TEMP TABLE checks(step TEXT,ok INTEGER NOT NULL CHECK(ok=1));
CREATE TEMP VIEW oracle AS SELECT l.farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm=p.farm AND l.crop=p.crop WHERE l.crates>=5 GROUP BY l.farmer;
CREATE TEMP VIEW verified AS SELECT
    NOT EXISTS(SELECT * FROM income EXCEPT SELECT * FROM oracle)
    AND NOT EXISTS(SELECT * FROM oracle EXCEPT SELECT * FROM income)
    AND (SELECT COUNT(*) FROM income)=(SELECT COUNT(*) FROM oracle) AS ok;
INSERT INTO checks SELECT 'reconnect renamed table',ok FROM verified;
INSERT INTO lots VALUES(3,1,10,101,8);
INSERT INTO checks SELECT 'insert after reconnect',ok FROM verified;
UPDATE prices SET dollars=25 WHERE id=1;
INSERT INTO checks SELECT 'joined price update',ok FROM verified;
DELETE FROM lots WHERE id=2;
INSERT INTO checks SELECT 'delete after reconnect',ok FROM verified;
SAVEPOINT caller;
ALTER TABLE income RENAME TO receipts;
UPDATE lots SET crates=3 WHERE id=1;
ROLLBACK TO caller;
RELEASE caller;
INSERT INTO checks SELECT 'rename savepoint rollback',ok FROM verified;
DROP VIEW temp.verified;
BEGIN;
DROP TABLE income;
ROLLBACK;
UPDATE lots SET crates=9 WHERE id=1;
INSERT INTO checks SELECT 'drop rollback restored hooks',COUNT(*)=1 AND MIN(n)=2 AND MIN(dollars)=425 FROM income;
DROP TABLE income;
INSERT INTO checks SELECT 'drop removes owned objects',
    (SELECT COUNT(*) FROM __ivm_objects)=0 AND (SELECT COUNT(*) FROM __ivm_views)=0
    AND NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='income' OR name GLOB 'income_*' OR name GLOB '__ivm_earnings_*' OR name GLOB '__ivm_income_*');
INSERT INTO checks SELECT 'source rows preserved',COUNT(*)=2 AND SUM(crates)=17 FROM lots;
SELECT step,ok FROM checks;
.print 'PASS: native CREATE, ALTER, DROP, defensive writes, rollback and reconnect'
SQL
