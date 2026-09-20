#!/usr/bin/env bash
set -euo pipefail
# usage: scripts/statement-costs.sh <test name> [label]; log kept at plans/costs/<label>.json
test_name="${1:?test name}"
label="${2:-$test_name}"
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/plans/costs"
mkdir -p "$out"
log="$out/$label.json"

start=$(date +%s.%N)
HAFLEY_LOG_FORMAT=json HAFLEY_LOG=sqlite=debug \
  cargo test -q --manifest-path "$root/Cargo.toml" --test "$test_name" 2>"$log" >/dev/null
end=$(date +%s.%N)
grep -a '^{"timestamp"' "$log" >"$log.tmp" && mv "$log.tmp" "$log"

echo "== $label: $(echo "$end - $start" | bc) s wall including cargo, $(wc -l <"$log") events"
# sqlite3_stmt_status is cumulative per handle and the trace callback cannot
# reset it; the delta against the previous event of the same handle is the cost.
duckdb -c "
CREATE TABLE events AS
SELECT
       coalesce(span.kind, 'outside_drain') AS kind,
       fields.sql AS sql, fields.nanos AS nanos, fields.vm_step AS total, fields.run AS run
FROM read_json('$log', format='newline_delimited', union_by_name=true)
WHERE fields.message = 'statement finished';
CREATE TABLE costs AS
SELECT *, CASE WHEN total >= prior THEN total - prior ELSE total END AS vm_step
FROM (SELECT *, coalesce(lag(total) OVER (PARTITION BY sql ORDER BY run), 0) AS prior FROM events);
SELECT kind, count(*) AS statements, sum(vm_step) AS vm_step, round(sum(nanos)/1e6, 1) AS ms
FROM costs GROUP BY 1 ORDER BY ms DESC;
SELECT kind, count(*) AS runs, round(avg(vm_step)) AS vm_step_per_run,
       round(sum(nanos)/1e6, 1) AS ms,
       left(regexp_replace(sql, '\s+', ' ', 'g'), 110) AS sql
FROM costs GROUP BY kind, sql ORDER BY sum(vm_step) DESC LIMIT 6;
"
