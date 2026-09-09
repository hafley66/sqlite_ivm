#!/usr/bin/env bash
# Persist the catalog, reopen to drop, then reopen with the module to write.
set -euo pipefail
ivm_extension=${1:?Pass the native SQLite extension path}
[[ -f "$ivm_extension" ]] || { printf 'Extension file missing: %s\n' "$ivm_extension" >&2; exit 1; }
ivm_extension_dot=${ivm_extension//\\/\\\\}
ivm_extension_dot=${ivm_extension_dot//\"/\\\"}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-lifecycle.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT
ivm_sqlite=${SQLITE3:-sqlite3}

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TABLE lots(id INTEGER PRIMARY KEY,farm_id INTEGER NOT NULL,crop_id INTEGER NOT NULL,farmer INTEGER NOT NULL,crates INTEGER NOT NULL);
CREATE TABLE prices(id INTEGER PRIMARY KEY,farm_id INTEGER NOT NULL,crop_id INTEGER NOT NULL,dollars INTEGER NOT NULL);
INSERT INTO lots VALUES(1,1,10,101,6),(2,1,10,102,8);
INSERT INTO prices VALUES(1,1,10,25);
SELECT sqlite_ivm_create('earnings', '
    SELECT l.farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm_id=p.farm_id AND l.crop_id=p.crop_id GROUP BY l.farmer');
SELECT sqlite_ivm_create('eligible_earnings', '
    SELECT l.farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm_id=p.farm_id AND l.crop_id=p.crop_id
    WHERE l.crates>=7 GROUP BY l.farmer');
SQL

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TEMP TABLE checks(step TEXT,ok INTEGER NOT NULL CHECK(ok=1));
CREATE TEMP TABLE before_schema AS SELECT type,name,sql FROM main.sqlite_schema;
CREATE TEMP VIEW eligible_oracle AS
    SELECT l.farmer,COUNT(*) AS n,SUM(l.crates*p.dollars) AS dollars
    FROM lots l JOIN prices p ON l.farm_id=p.farm_id AND l.crop_id=p.crop_id
    WHERE l.crates>=7 GROUP BY l.farmer;
CREATE TEMP VIEW verified AS SELECT
    NOT EXISTS(SELECT * FROM eligible_earnings EXCEPT SELECT * FROM eligible_oracle)
    AND NOT EXISTS(SELECT * FROM eligible_oracle EXCEPT SELECT * FROM eligible_earnings)
    AND (SELECT COUNT(*) FROM eligible_earnings)=(SELECT COUNT(*) FROM eligible_oracle) AS ok;
INSERT INTO checks SELECT 'persisted catalog',COUNT(*)=2 FROM __ivm_views;
BEGIN;
SELECT sqlite_ivm_drop('EARNINGS');
INSERT INTO checks SELECT 'owned objects removed',COUNT(*)=0 FROM sqlite_schema
    WHERE name='earnings' OR name GLOB 'earnings_*' OR name GLOB '__ivm_earnings_*';
INSERT INTO checks SELECT 'sources preserved',(SELECT COUNT(*) FROM lots)=2 AND (SELECT SUM(crates) FROM lots)=14
    AND (SELECT COUNT(*) FROM prices)=1 AND (SELECT SUM(dollars) FROM prices)=25;
UPDATE prices SET dollars=30;
INSERT INTO checks SELECT 'survivor after drop',ok FROM verified;
ROLLBACK;
INSERT INTO checks SELECT 'schema restored',
    NOT EXISTS(SELECT * FROM before_schema EXCEPT SELECT type,name,sql FROM main.sqlite_schema)
    AND NOT EXISTS(SELECT type,name,sql FROM main.sqlite_schema EXCEPT SELECT * FROM before_schema);
INSERT INTO checks SELECT 'catalog restored',COUNT(*)=2 FROM __ivm_views;
UPDATE prices SET dollars=30;
INSERT INTO checks SELECT 'restored triggers work',COUNT(*)=2 AND SUM(n)=2 AND SUM(dollars)=420 FROM earnings;
INSERT INTO checks SELECT 'surviving triggers work',ok FROM verified;
SELECT sqlite_ivm_drop('earnings');
INSERT INTO checks SELECT 'drop committed',COUNT(*)=0 FROM sqlite_schema
    WHERE name='earnings' OR name GLOB 'earnings_*' OR name GLOB '__ivm_earnings_*';
INSERT INTO checks SELECT 'metadata removed',
    NOT EXISTS(SELECT 1 FROM __ivm_views WHERE name='earnings')
    AND NOT EXISTS(SELECT 1 FROM __ivm_sources WHERE view_name='earnings')
    AND NOT EXISTS(SELECT 1 FROM __ivm_columns WHERE view_name='earnings')
    AND NOT EXISTS(SELECT 1 FROM __ivm_objects WHERE view_name='earnings');
SELECT step,ok FROM checks;
SQL

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
CREATE TEMP TABLE checks(step TEXT,ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO checks SELECT 'module loaded',EXISTS(
    SELECT 1 FROM pragma_module_list WHERE name='sqlite_ivm');
UPDATE lots SET crates=9 WHERE id=1;
INSERT INTO checks SELECT 'fresh writer filter entry',COUNT(*)=2 AND SUM(n)=2 AND SUM(dollars)=510 FROM eligible_earnings;
DELETE FROM lots WHERE id=2;
INSERT INTO checks SELECT 'fresh writer delete',COUNT(*)=1 AND MIN(farmer)=101 AND MIN(n)=1 AND MIN(dollars)=270 FROM eligible_earnings;
SELECT step,ok FROM checks;
SQL

"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
CREATE TEMP TABLE checks(ok INTEGER NOT NULL CHECK(ok=1));
SELECT sqlite_ivm_drop('eligible_earnings');
INSERT INTO checks SELECT (SELECT COUNT(*) FROM __ivm_views)=0
    AND (SELECT COUNT(*) FROM __ivm_sources)=0 AND (SELECT COUNT(*) FROM __ivm_columns)=0
    AND (SELECT COUNT(*) FROM __ivm_objects)=0;
INSERT INTO checks SELECT COUNT(*)=1 AND MIN(crates)=9 FROM lots;
.print 'PASS: persistent catalog, managed drop, rollback, surviving view and fresh connection writes'
SQL
