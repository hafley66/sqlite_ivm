#!/usr/bin/env bash
set -euo pipefail
lab_dir=$(cd "$(dirname "$0")" && pwd)
: "${CARGO_TARGET_DIR:?dedicated lane target required}"
: "${1:?fresh receipt directory required}"
if [[ -e "$1" ]]; then printf 'refusing existing gate receipt: %s\n' "$1" >&2; exit 2; fi
mkdir -p "$1"
receipt=$(cd "$1" && pwd)
case "$(uname -s)" in Darwin) extension_suffix=dylib ;; *) extension_suffix=so ;; esac
export SQLITE_IVM_EXTENSION="$CARGO_TARGET_DIR/release/libsqlite_ivm.$extension_suffix"
export SEMANTIC_DD_BIN="$CARGO_TARGET_DIR/release/examples/semantic_dd"
export CIRCUIT_DD_BIN="$CARGO_TARGET_DIR/release/examples/circuit_dd"
export CROSSOVER_DD_BIN="$CARGO_TARGET_DIR/release/examples/crossover_dd"
export SQLITE_IVM_FAILURE_ROOT="$receipt/failing-traces"
export SQLITE_TEMPLATE_PROGRAM="$lab_dir/results/template-reuse-20260908/18_crossover.rs"
export SQLITE_TEMPLATE_FIXTURE="$lab_dir/results/template-reuse-20260908/smoke/constrained-sqlite-template-group-400:10:10-measured-1/fixture.json"
cargo build --locked --release --manifest-path "$lab_dir/25_sqlite_ivm/Cargo.toml" > "$receipt/plugin-build.log" 2>&1
cargo build --locked --offline --release --manifest-path "$lab_dir/../../../sprefa-store/Cargo.toml" --example crossover_dd --example circuit_dd --example semantic_dd > "$receipt/dd-build.log" 2>&1
python3 "$lab_dir/27_sqlite_ivm.test.py" -v > "$receipt/plugin-tests.log" 2>&1
python3 "$lab_dir/20_sqlite_template.test.py" -v > "$receipt/template-tests.log" 2>&1
python3 "$lab_dir/17_sqlite_trigger_capabilities.py" -v > "$receipt/mechanism-tests.log" 2>&1
node --test "$lab_dir/40_semantics.test.mjs" > "$receipt/semantic-tests.log" 2>&1
node --test "$lab_dir/33_circuit.test.mjs" > "$receipt/circuit-tests.log" 2>&1
node --test "$lab_dir/23_crossover.test.mjs" > "$receipt/integration-tests.log" 2>&1
cargo test --locked --offline --release --manifest-path "$lab_dir/../../../sprefa-store/Cargo.toml" --test oracle_dd --test datalog_ops --test circuit_semantic_graphs > "$receipt/store-reference-tests.log" 2>&1
printf 'PASS: loaded plugin, existing SQLite mechanisms/templates, shared DD/plugin integration and store reference tests\n' > "$receipt/status.txt"
cat "$receipt/status.txt"
