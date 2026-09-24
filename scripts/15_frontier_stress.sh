#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
root="$PWD"
extension_manifest="$root/labs/20260923.2.the-gang-builds-the-sqlite-frontier-engine/crates/frontier-ext/Cargo.toml"
extension_target="$root/target/frontier-ext"
CARGO_TARGET_DIR="$extension_target" cargo build --offline --release --manifest-path "$extension_manifest"
dd_extension_manifest="$root/labs/20260923.3.dd-inside-sqlite/ext/Cargo.toml"
dd_extension_target="$root/target/frontier-dd-ext"
CARGO_TARGET_DIR="$dd_extension_target" cargo build --offline --release --manifest-path "$dd_extension_manifest"

case "$(uname -s)" in
    Darwin) suffix=dylib ;;
    Linux) suffix=so ;;
    *) echo "unsupported extension library platform" >&2; exit 1 ;;
esac

FRONTIER_EXT_PATH="$extension_target/release/libfrontier_ext.$suffix" \
    FRONTIER_DD_EXT_PATH="$dd_extension_target/release/libfrontier_dd_ext.$suffix" \
    CARGO_TARGET_DIR="$root/target" \
    cargo run --offline --release --manifest-path bench/Cargo.toml --bin frontier-stress -- "$@"
