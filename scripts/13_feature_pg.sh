#!/usr/bin/env bash
set -euo pipefail
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ivm_output=${1:?pass the existing feature artifact directory}
ivm_prefix=${IVM_POSTGRES_PREFIX:?set IVM_POSTGRES_PREFIX to a PostgreSQL prefix with pg_ivm installed}
ivm_work=$(mktemp -d /tmp/ivm-features.XXXXXX)
cleanup(){ "$ivm_prefix/bin/pg_ctl" -D "$ivm_work/data" -m immediate stop >/dev/null 2>&1 || true; rm -rf -- "$ivm_work"; }
trap cleanup EXIT
"$ivm_prefix/bin/initdb" -D "$ivm_work/data" --auth=trust --no-locale --encoding=UTF8 >/dev/null
mkdir -p "$ivm_work/socket"
"$ivm_prefix/bin/pg_ctl" -D "$ivm_work/data" -l "$ivm_work/postgres.log" -o "-c listen_addresses='' -c unix_socket_directories='$ivm_work/socket' -c shared_preload_libraries='pg_ivm' -c shared_buffers=32MB -c work_mem=1MB -c temp_file_limit=128MB -c statement_timeout=30000 -c max_connections=16" start -w >/dev/null
export PGHOST="$ivm_work/socket" PGPORT=5432 PGDATABASE=postgres
export PGUSER
PGUSER=$(id -un)
node "$ivm_dir/bench/42_feature_run.mjs" pg "$ivm_output/cases" | tee "$ivm_output/pg.jsonl"
