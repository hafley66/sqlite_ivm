#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_extension=$(bash "$ivm_dir/scripts/0_build.sh" release)
ivm_bench_target="$ivm_dir/target"
CARGO_TARGET_DIR="$ivm_bench_target" cargo build --offline --locked --release --manifest-path "$ivm_dir/bench/Cargo.toml"
IVM_EXTENSION="$ivm_extension" "$ivm_bench_target/release/bench" shootout "$@"
