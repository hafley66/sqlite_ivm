#!/usr/bin/env bash
set -euo pipefail
ivm_extension=${1:?Pass the native SQLite extension path}
ivm_extension_dot=${ivm_extension//\\/\\\\}
ivm_extension_dot=${ivm_extension_dot//\"/\\\"}
ivm_test_dir=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-source-ddl.XXXXXX")
trap 'rm -rf -- "$ivm_test_dir"' EXIT
ivm_sqlite=${SQLITE3:-sqlite3}
"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
.dbconfig defensive on
CREATE TABLE farms(id INTEGER PRIMARY KEY,region TEXT,price INTEGER);
INSERT INTO farms VALUES(1,'north',20),(2,'south',NULL);
CREATE VIRTUAL TABLE earnings USING sqlite_ivm('SELECT region AS place,COUNT(*) AS n,SUM(price) AS money,AVG(price) AS mean FROM farms GROUP BY region');
SELECT sqlite_ivm_rename_source('farms','growers');
SELECT sqlite_ivm_rename_column('growers','price','cost');
UPDATE growers SET cost=30 WHERE id=1;
BEGIN;
SELECT sqlite_ivm_rename_source('growers','orchards');
DELETE FROM orchards;
ROLLBACK;
SQL
"$ivm_sqlite" -batch -bail "$ivm_test_dir/test.sqlite" <<SQL
.load "$ivm_extension_dot"
PRAGMA recursive_triggers=ON;
PRAGMA trusted_schema=ON;
.dbconfig defensive on
CREATE TEMP TABLE checks(step TEXT,ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO checks SELECT 'reopen source DDL',count(*)=2 AND sum(money)=30 FROM earnings;
UPDATE growers SET cost=40 WHERE id=2;
INSERT INTO checks SELECT 'maintain rewritten query',count(*)=2 AND sum(money)=70 FROM earnings;
ALTER TABLE earnings RENAME TO income;
BEGIN;
SELECT sqlite_ivm_drop_source('growers',1);
ROLLBACK;
INSERT INTO checks SELECT 'cascade rollback',count(*)=2 AND sum(money)=70 FROM income;
SELECT sqlite_ivm_drop_source('growers',1);
INSERT INTO checks SELECT 'cascade cleanup',count(*)=0 FROM __ivm_views;
SELECT step,ok FROM checks;
.print 'PASS: native source rename, column rename, cascade, rollback and reopen'
SQL
