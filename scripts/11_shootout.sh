#!/usr/bin/env bash
# Run after scripts/9_verify.sh. PostgreSQL is needed only when its arm is selected.
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_output=${1:?Pass a new absolute receipt.jsonl path}
shift
cargo build --locked --release --manifest-path "$ivm_dir/bench/Cargo.toml"
case "$(uname -s)" in Darwin) ivm_library=libsqlite_ivm.dylib;; Linux) ivm_library=libsqlite_ivm.so;; *) exit 2;; esac
ivm_args=(--arms "${IVM_ARMS:-sqlite-plugin-delta,pg_ivm,dd,sqlite-query}" --sqlite-bin "$ivm_dir/target/release/examples/4_sqlite_case" --sqlite-extension "$ivm_dir/target/release/$ivm_library" --circuit-dd-bin "$ivm_dir/bench/target/release/circuit_dd" --semantic-dd-bin "$ivm_dir/bench/target/release/semantic_dd" "$@")
if [[ ${IVM_ARMS:-pg_ivm} == *pg_ivm* ]]; then
  bash "$ivm_dir/bench/shared/13_crossover_run.sh" circuits "$ivm_output" "${ivm_args[@]}"
else
  [[ ! -e "$ivm_output" ]] || { printf 'Receipt already exists\n' >&2;exit 2; }
  export IVM_RUN_ROOT="${ivm_output%.jsonl}.artifacts"
  node "$ivm_dir/bench/shared/12_crossover_runner.mjs" --profile circuits --output "$ivm_output" "${ivm_args[@]}"
fi
