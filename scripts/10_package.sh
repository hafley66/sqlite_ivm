#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo build --locked --release --no-default-features --features extension --manifest-path "$ivm_dir/Cargo.toml" --target-dir "$ivm_dir/target/extension"
ivm_version=$(cargo metadata --no-deps --format-version 1 --manifest-path "$ivm_dir/Cargo.toml" | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "sqlite-ivm"))')
case "$(uname -s)" in Darwin) ivm_library=libsqlite_ivm.dylib;; Linux) ivm_library=libsqlite_ivm.so;; *) printf 'Unsupported archive platform\n' >&2;exit 2;; esac
ivm_archive="sqlite-ivm-$ivm_version-$(uname -s)-$(uname -m)"
ivm_stage=$(mktemp -d "${TMPDIR:-/tmp}/sqlite-ivm-package.XXXXXX")
trap 'rm -rf -- "$ivm_stage"' EXIT
mkdir -p "$ivm_stage/$ivm_archive" "$ivm_dir/dist"
cp -f "$ivm_dir/target/extension/release/$ivm_library" "$ivm_dir/README.md" "$ivm_dir/LICENSE-MIT" "$ivm_dir/LICENSE-APACHE" "$ivm_stage/$ivm_archive/"
tar -czf "$ivm_dir/dist/$ivm_archive.tar.gz" -C "$ivm_stage" "$ivm_archive"
(cd "$ivm_dir/dist" && shasum -a 256 "$ivm_archive.tar.gz" > "$ivm_archive.tar.gz.sha256")
printf '%s\n' "$ivm_dir/dist/$ivm_archive.tar.gz"
# A source archive carries the standalone component and its separate DD harness.
ivm_source="sqlite-ivm-$ivm_version-source"
git -C "$ivm_dir" archive --format=tar.gz --prefix="$ivm_source/" HEAD > "$ivm_dir/dist/$ivm_source.tar.gz"
(cd "$ivm_dir/dist" && shasum -a 256 "$ivm_source.tar.gz" > "$ivm_source.tar.gz.sha256")
printf '%s\n' "$ivm_dir/dist/$ivm_source.tar.gz"
