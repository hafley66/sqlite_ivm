#!/usr/bin/env bash
# Build, execute semantic coverage, then measure validated circuits and report.
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec node "$ivm_dir/bench/53_shootout.mjs" "$@"
