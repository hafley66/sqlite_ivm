#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_extension=$(bash "$ivm_dir/scripts/0_build.sh" release)
CARGO_TARGET_DIR="$ivm_dir/target" cargo build --offline --locked --release --manifest-path "$ivm_dir/bench/Cargo.toml" --bin crossover-dd
python3 "$ivm_dir/bench/crossover/28_run.py" --extension "$ivm_extension" --dd-bin "$ivm_dir/target/release/crossover-dd" "$@"
