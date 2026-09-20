#!/usr/bin/env bash

# B-tree write rate per arrangement key shape, end to end through sqlite3 and
# the loadable extension. One label per run so `git log` reads as a series.
set -euo pipefail

probe_label=${1:?Usage: measure.sh <label> [rows]}
probe_rows=${2:-4000}
probe_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ivm_dir=$(cd -- "$probe_dir/../.." && pwd)

# The macOS system sqlite3 is built without load_extension, so the probe needs
# the brew build that CI also pins through SQLITE3.
: "${SQLITE3:=$(brew --prefix sqlite 2>/dev/null)/bin/sqlite3}"
ivm_extension=$(bash "$ivm_dir/scripts/0_build.sh")
ivm_extension_sql=${ivm_extension//\'/\'\'}
probe_work=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-intern.XXXXXX")
trap 'rm -rf -- "$probe_work"' EXIT

# Real seconds out of sqlite3's own .timer, so the number covers the trigger,
# the vtab dispatch and every arrangement write, and nothing in this script.
probe_run() {
  local shape=$1 schema=$2 view=$3 load=$4
  local db="$probe_work/$shape.sqlite"
  local out
  out=$("$SQLITE3" -batch -bail "$db" <<SQL
SELECT load_extension('$ivm_extension_sql');
PRAGMA recursive_triggers = ON;
PRAGMA trusted_schema = ON;
$schema
$view
.timer on
$load
.timer off
SQL
)
  local seconds
  seconds=$(printf '%s\n' "$out" | awk '/^Run Time:/{print $4}' | tail -1)
  local bytes
  bytes=$(wc -c <"$db" | tr -d ' ')
  printf '%s\t%s\t%s\t%s\t%s\n' "$probe_label" "$shape" "$probe_rows" "$seconds" "$bytes" \
    | awk -F'\t' '{printf "| %s | %s | %s | %.3f | %.0f | %.1f |\n",$1,$2,$3,$4,$5,$3/$4}'
}

probe_series="SELECT value AS i FROM generate_series(1,$probe_rows)"

printf '| label | shape | rows | seconds | db bytes | rows/s |\n|---|---|---|---|---|---|\n'

probe_run group \
  "CREATE TABLE src(id INTEGER PRIMARY KEY,k TEXT NOT NULL,j TEXT NOT NULL,v INTEGER NOT NULL);" \
  "SELECT sqlite_ivm_create('totals','SELECT k, COUNT(*) AS n, SUM(v) AS s FROM src GROUP BY k');" \
  "INSERT INTO src(id,k,j,v) SELECT i,'group-key-'||(i%97),'other-'||(i%53),i FROM ($probe_series);"

probe_run distinct \
  "CREATE TABLE src(id INTEGER PRIMARY KEY,k TEXT NOT NULL,j TEXT NOT NULL,v INTEGER NOT NULL);" \
  "SELECT sqlite_ivm_create('pairs','SELECT DISTINCT k, j FROM src');" \
  "INSERT INTO src(id,k,j,v) SELECT i,'group-key-'||(i%97),'other-'||(i%53),i FROM ($probe_series);"

probe_run join \
  "CREATE TABLE src(id INTEGER PRIMARY KEY,k TEXT NOT NULL,j TEXT NOT NULL,v INTEGER NOT NULL);
   CREATE TABLE dim(id INTEGER PRIMARY KEY,k TEXT NOT NULL,label TEXT NOT NULL);
   INSERT INTO dim(id,k,label) SELECT value,'group-key-'||value,'label-'||value FROM generate_series(0,96);" \
  "SELECT sqlite_ivm_create('joined','SELECT src.k AS k, src.v AS v, dim.label AS label FROM src JOIN dim ON dim.k = src.k');" \
  "INSERT INTO src(id,k,j,v) SELECT i,'group-key-'||(i%97),'other-'||(i%53),i FROM ($probe_series);"

# Interning priced on its own: every composite is distinct, so this is the
# worst case of one dictionary row and one UNIQUE index entry per key.
probe_run intern \
  "CREATE TABLE src(id INTEGER PRIMARY KEY,k TEXT NOT NULL,j TEXT NOT NULL,v INTEGER NOT NULL);" \
  "SELECT sqlite_ivm_create('totals','SELECT k, COUNT(*) AS n FROM src GROUP BY k');" \
  "INSERT OR IGNORE INTO totals_keys(__v) SELECT json_array('group-key-'||i,'other-'||(i%53),i) FROM ($probe_series);"

# The member table is the third key shape: UNIQUE __k, no __r. Edges stay sparse
# so the closure is linear in inserted rows rather than quadratic.
probe_run fixpoint \
  "CREATE TABLE edge(id INTEGER PRIMARY KEY,a TEXT NOT NULL,b TEXT NOT NULL);" \
  "SELECT sqlite_ivm_create('reached','WITH RECURSIVE p(x,y) AS (SELECT a,b FROM edge UNION SELECT p.x,edge.b FROM p JOIN edge ON edge.a=p.y) SELECT x,y FROM p');" \
  "INSERT INTO edge(id,a,b) SELECT i,'node-'||i,'node-'||(i+1) FROM ($probe_series) WHERE i%64!=0;"
