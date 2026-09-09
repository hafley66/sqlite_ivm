#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
export IVM_NATIVE_EXTENSION=${1:?pass the built native extension path}
cargo test --locked --features bench --manifest-path "$ivm_dir/Cargo.toml" --test 4_features --test 5_transactions
