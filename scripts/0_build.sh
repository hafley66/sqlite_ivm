#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --offline --locked --manifest-path "$ivm_dir/Cargo.toml" \
  --features extension --target-dir "$ivm_dir/target"
case "$(uname -s)" in
  Darwin) printf '%s\n' "$ivm_dir/target/debug/libsqlite_ivm.dylib" ;;
  Linux) printf '%s\n' "$ivm_dir/target/debug/libsqlite_ivm.so" ;;
  *) printf '%s\n' "$ivm_dir/target/debug/sqlite_ivm.dll" ;;
esac
