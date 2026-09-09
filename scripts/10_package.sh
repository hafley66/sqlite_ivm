#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --locked --release --features extension --manifest-path "$ivm_dir/Cargo.toml"
ivm_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ivm_dir/Cargo.toml" | head -n 1)
case "$(uname -s)" in Darwin) ivm_library=libsqlite_ivm.dylib;; Linux) ivm_library=libsqlite_ivm.so;; *) printf 'Unsupported archive platform\n' >&2;exit 2;; esac
ivm_archive="sqlite-ivm-$ivm_version-$(uname -s)-$(uname -m)"
ivm_stage=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-package.XXXXXX")
trap 'rm -rf -- "$ivm_stage"' EXIT
mkdir -p "$ivm_stage/$ivm_archive" "$ivm_dir/dist"
cp -f "$ivm_dir/target/release/$ivm_library" "$ivm_dir/README.md" "$ivm_dir/LICENSE-MIT" "$ivm_dir/LICENSE-APACHE" "$ivm_stage/$ivm_archive/"
tar -czf "$ivm_dir/dist/$ivm_archive.tar.gz" -C "$ivm_stage" "$ivm_archive"
(cd "$ivm_dir/dist" && shasum -a 256 "$ivm_archive.tar.gz" > "$ivm_archive.tar.gz.sha256")
printf '%s\n' "$ivm_dir/dist/$ivm_archive.tar.gz"
# A source archive carries the standalone component and its separate DD harness.
ivm_source="sqlite-ivm-$ivm_version-source"
mkdir -p "$ivm_stage/$ivm_source/.github/workflows"
tar -cf "$ivm_stage/source.tar" -C "$ivm_dir/.." \
  --exclude='sqlite_ivm/target' --exclude='sqlite_ivm/bench/target' \
  --exclude='sqlite_ivm/bench/shared/node_modules' \
  --exclude='sqlite_ivm/bench/results' --exclude='sqlite_ivm/dist' sqlite_ivm
tar -xf "$ivm_stage/source.tar" -C "$ivm_stage/$ivm_source"
cp -f "$ivm_dir/../.github/workflows/sqlite-ivm.yml" "$ivm_stage/$ivm_source/.github/workflows/"
tar -czf "$ivm_dir/dist/$ivm_source.tar.gz" -C "$ivm_stage" "$ivm_source"
(cd "$ivm_dir/dist" && shasum -a 256 "$ivm_source.tar.gz" > "$ivm_source.tar.gz.sha256")
printf '%s\n' "$ivm_dir/dist/$ivm_source.tar.gz"
