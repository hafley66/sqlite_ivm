#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --locked --release --features bench --example 4_sqlite_case --manifest-path "$ivm_dir/Cargo.toml"
cargo build --locked --release --features extension --manifest-path "$ivm_dir/Cargo.toml"
case "$(uname -s)" in Darwin) ivm_library=libsqlite_ivm.dylib;; Linux) ivm_library=libsqlite_ivm.so;; *) exit 2;; esac
node "$ivm_dir/scripts/7_native_shared.mjs" "$ivm_dir/target/release/examples/4_sqlite_case" "$ivm_dir/target/release/$ivm_library"
