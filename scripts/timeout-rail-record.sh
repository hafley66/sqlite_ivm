#!/usr/bin/env bash
# Refresh scripts/timeout-rail.tsv from four battery runs, per-test median of
# the last three. The first run is a warmup and is discarded: a cold target dir
# or a cold page cache swings the first run far above the rest, and a baseline
# taken from it would fire on a later warm run. Run this on a quiet machine at
# the sha whose walls are to be recorded, then commit the file. See
# plans/costs/timeout-rail.md for the raw runs.
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
rail_config="$ivm_dir/scripts/nextest.toml"
rail_junit="$ivm_dir/target/nextest/default/junit.xml"
platform=$(uname -sm)
work=$(mktemp -d "${TMPDIR:-/tmp}/timeout-rail-record.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

for run in 1 2 3 4; do
  rm -f "$rail_junit"
  cargo nextest run --locked --no-fail-fast --manifest-path "$ivm_dir/Cargo.toml" \
    --config-file "$rail_config"
  if [ ! -f "$rail_junit" ]; then
    printf 'record: no JUnit report at %s\n' "$rail_junit" >&2
    exit 1
  fi
  if [ "$run" -eq 1 ]; then
    printf 'record: run 1 was the warmup and is discarded\n' >&2
    continue
  fi
  cp "$rail_junit" "$work/run$run.xml"
done

python3 "$ivm_dir/scripts/timeout-rail.py" record "$platform" \
  "$ivm_dir/scripts/timeout-rail.tsv" "$work"/run2.xml "$work"/run3.xml "$work"/run4.xml