#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo test --locked --manifest-path "$ivm_dir/Cargo.toml"
ivm_extension=$(bash "$ivm_dir/scripts/0_build.sh")
for ivm_scenario in "$ivm_dir"/scripts/[1-6]_*.sh; do
  bash "$ivm_scenario" "$ivm_extension"
done
bash "$ivm_dir/scripts/8_native_shared.sh"
