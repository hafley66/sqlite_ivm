#!/usr/bin/env bash
# The gate. Correctness was never the gap; the clock is the addition.
#
# Level 1: per-test budgets, scripts/nextest.toml, enforced by cargo nextest's
#          slow-timeout. A test over its budget is killed and named.
# Level 2: one ceiling on the whole battery, the timeout(1) call below.
# Level 3: each test's wall and the battery wall against the recorded row for
#          this platform in scripts/timeout-rail.tsv.
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [ -z "${SQLITE3:-}" ] && [ "$(uname -s)" = Darwin ] && command -v brew >/dev/null; then
  ivm_sqlite_prefix=$(brew --prefix sqlite)
  if [ -x "$ivm_sqlite_prefix/bin/sqlite3" ]; then
    export SQLITE3="$ivm_sqlite_prefix/bin/sqlite3"
  fi
fi
rail_config="$ivm_dir/scripts/nextest.toml"
rail_target=$(cargo metadata --no-deps --format-version 1 --manifest-path "$ivm_dir/Cargo.toml" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
rail_junit="$rail_target/nextest/default/junit.xml"
rail_tsv="$ivm_dir/scripts/timeout-rail.tsv"
# 120s is 13.5x the 8.857s recorded battery wall: room for a slower runner, and
# still a ceiling on a stall. Override for a proof or a slower host.
rail_ceiling=${TIMEOUT_RAIL_CEILING:-120}

if ! command -v cargo-nextest >/dev/null; then
  printf 'gate: cargo-nextest is required (cargo install cargo-nextest --locked)\n' >&2
  exit 1
fi
if ! command -v python3 >/dev/null; then
  printf 'gate: python3 is required by scripts/timeout-rail.py\n' >&2
  exit 1
fi
ivm_timeout=
for tool in timeout gtimeout; do
  if command -v "$tool" >/dev/null; then
    ivm_timeout=$tool
    break
  fi
done
if [ -z "$ivm_timeout" ]; then
  printf 'gate: coreutils timeout(1) is required (brew install coreutils)\n' >&2
  exit 1
fi

# Build first, so the ceiling covers the run and not the compile.
rm -f "$rail_junit"
cargo nextest run --locked --manifest-path "$ivm_dir/Cargo.toml" \
  --config-file "$rail_config" --no-run

status=0
"$ivm_timeout" --kill-after=5s "$rail_ceiling" \
  cargo nextest run --locked --no-fail-fast --manifest-path "$ivm_dir/Cargo.toml" \
  --config-file "$rail_config" || status=$?
if [ "$status" -ne 0 ]; then
  if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
    printf 'gate: battery exceeded the %ss ceiling\n' "$rail_ceiling" >&2
  fi
  exit "$status"
fi

python3 "$ivm_dir/scripts/timeout-rail.py" check "$(uname -sm)" "$rail_tsv" "$rail_junit"

ivm_extension=$(bash "$ivm_dir/scripts/0_build.sh")
IVM_EXTENSION="$ivm_extension" CARGO_TARGET_DIR="$rail_target" \
  cargo test --offline --locked --release --manifest-path "$ivm_dir/bench/Cargo.toml" --test 6_extension_load
for ivm_scenario in "$ivm_dir"/scripts/[1-6]_*.sh; do
  bash "$ivm_scenario" "$ivm_extension"
done
IVM_NATIVE_EXTENSION="$ivm_extension" SQLITE3="${SQLITE3:-sqlite3}" \
  cargo test --offline --locked --manifest-path "$ivm_dir/Cargo.toml" --test 12_compass compass_passes_through_the_cli
