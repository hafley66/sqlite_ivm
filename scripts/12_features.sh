#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_output=${1:?pass a new artifact directory}
if [[ -e "$ivm_output" ]]; then printf 'artifact directory already exists: %s\n' "$ivm_output" >&2; exit 2; fi
mkdir -p "$ivm_output"
ivm_output=$(cd "$ivm_output" && pwd)
cargo build --locked --release --features bench --example 5_feature_case --manifest-path "$ivm_dir/Cargo.toml"
cargo build --locked --release --features extension --manifest-path "$ivm_dir/Cargo.toml"
cargo build --locked --release --bin feature_dd --manifest-path "$ivm_dir/bench/Cargo.toml"
case "$(uname -s)" in Darwin) ivm_library=libsqlite_ivm.dylib;; Linux) ivm_library=libsqlite_ivm.so;; *) exit 2;; esac
"$ivm_dir/target/release/examples/5_feature_case" "$ivm_dir/tests/fixtures/1_features.json" "$ivm_dir/target/release/$ivm_library" "$ivm_output/cases" | tee "$ivm_output/native.jsonl"
node "$ivm_dir/bench/42_feature_run.mjs" dd "$ivm_output/cases" "$ivm_dir/bench/target/release/feature_dd" | tee "$ivm_output/dd.jsonl"
node "$ivm_dir/bench/45_feature_report.mjs" manifest "$ivm_output" "$ivm_dir/target/release/$ivm_library"
