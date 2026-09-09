#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_output=${1:?pass the feature artifact directory}
# The raw comparison returns failure for an observed pg_ivm mismatch.
# This separate regression check requires the exact recorded 1.15 behavior,
# including every mismatching row, while every ordinary PG query must pass.
ivm_status=0
bash "$ivm_dir/scripts/13_feature_pg.sh" "$ivm_output" || ivm_status=$?
if [[ "$ivm_status" -gt 1 ]]; then exit "$ivm_status"; fi
node "$ivm_dir/bench/45_feature_report.mjs" pg-baseline "$ivm_output"
