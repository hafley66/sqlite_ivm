#!/usr/bin/env bash
# Refresh scripts/timeout-rail.tsv from three battery runs, per-test median.
# Run it on a quiet machine at the sha whose walls are to be recorded, then
# commit the file. See plans/costs/timeout-rail.md for the raw runs.
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
rail_config="$ivm_dir/scripts/nextest.toml"
rail_junit="$ivm_dir/target/nextest/default/junit.xml"
platform=$(uname -sm)
work=$(mktemp -d "${TMPDIR:-/tmp}/timeout-rail-record.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

for run in 1 2 3; do
  rm -f "$rail_junit"
  cargo nextest run --locked --no-fail-fast --manifest-path "$ivm_dir/Cargo.toml" \
    --config-file "$rail_config"
  if [ ! -f "$rail_junit" ]; then
    printf 'record: no JUnit report at %s\n' "$rail_junit" >&2
    exit 1
  fi
  cp "$rail_junit" "$work/run$run.xml"
done

python3 "$ivm_dir/scripts/timeout-rail.py" record "$platform" \
  "$ivm_dir/scripts/timeout-rail.tsv" "$work"/run1.xml "$work"/run2.xml "$work"/run3.xml