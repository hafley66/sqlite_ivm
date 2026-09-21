#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_profile=${1:-debug}
ivm_flags=()
case "$ivm_profile" in
  debug) ;;
  release) ivm_flags+=(--release) ;;
  *) printf 'usage: %s [debug|release]\n' "$0" >&2; exit 2 ;;
esac
cargo build "${ivm_flags[@]}" --offline --locked --manifest-path "$ivm_dir/Cargo.toml" \
  --no-default-features --features extension --target-dir "$ivm_dir/target/extension"
case "$(uname -s)" in
  Darwin) printf '%s\n' "$ivm_dir/target/extension/$ivm_profile/libsqlite_ivm.dylib" ;;
  Linux) printf '%s\n' "$ivm_dir/target/extension/$ivm_profile/libsqlite_ivm.so" ;;
  *) printf '%s\n' "$ivm_dir/target/extension/$ivm_profile/sqlite_ivm.dll" ;;
esac
