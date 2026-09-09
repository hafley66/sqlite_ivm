#!/usr/bin/env bash
set -euo pipefail

lab_dir=$(cd "$(dirname "$0")" && pwd)
profile=${1:-smoke}
output=${2:-"$lab_dir/out/$profile.jsonl"}
postgres_prefix=${IVM_POSTGRES_PREFIX:-"$lab_dir/.work/postgres-18.6"}
run_root=$(mktemp -d "${TMPDIR:-/tmp}/sprefa-ivm.XXXXXX")
cluster_dir="$run_root/cluster"
socket_dir="$run_root/socket"
postgres_started=0

cleanup() {
  if [[ "$postgres_started" -eq 1 ]]; then
    "$postgres_prefix/bin/pg_ctl" -D "$cluster_dir" -m immediate stop >/dev/null 2>&1 || true
  fi
  case "$run_root" in
    "${TMPDIR:-/tmp}"/sprefa-ivm.*) rm -rf -- "$run_root" ;;
  esac
}
trap cleanup EXIT

case "$profile" in
  smoke|full) ;;
  *) printf 'usage: %s [smoke|full] [output.jsonl]\n' "$0" >&2; exit 2 ;;
esac

if [[ ! -d "$lab_dir/node_modules/@electric-sql/pglite" ]]; then
  printf 'missing task-local packages; run: cd %s && npm ci\n' "$lab_dir" >&2
  exit 2
fi

export IVM_RUN_ROOT="$run_root/cases"
export IVM_RSS_LIMIT_MB=${IVM_RSS_LIMIT_MB:-3072}

runner_args=(--profile "$profile" --output "$output")
if [[ "$profile" == "smoke" ]]; then
  runner_args+=(--warmups 0 --repetitions 1)
else
  runner_args+=(--warmups "${IVM_WARMUPS:-1}" --repetitions "${IVM_REPETITIONS:-3}")
fi

if [[ -x "$postgres_prefix/bin/postgres" && "${IVM_SKIP_NATIVE:-0}" != "1" ]]; then
  mkdir -p "$socket_dir"
  "$postgres_prefix/bin/initdb" -D "$cluster_dir" --auth=trust --no-locale --encoding=UTF8 >/dev/null
  export IVM_SHARED_BUFFERS=${IVM_SHARED_BUFFERS:-128MB}
  export IVM_WORK_MEM=${IVM_WORK_MEM:-16MB}
  export IVM_TEMP_FILE_LIMIT=${IVM_TEMP_FILE_LIMIT:-2048MB}
  postgres_options="-c listen_addresses='' -c unix_socket_directories='$socket_dir' -c shared_preload_libraries='pg_ivm' -c fsync=on -c synchronous_commit=on -c full_page_writes=on -c statement_timeout=120000 -c shared_buffers=$IVM_SHARED_BUFFERS -c work_mem=$IVM_WORK_MEM -c temp_file_limit=$IVM_TEMP_FILE_LIMIT"
  "$postgres_prefix/bin/pg_ctl" -D "$cluster_dir" -l "$run_root/postgres.log" -o "$postgres_options" start -w >/dev/null
  postgres_started=1
  export PGHOST="$socket_dir"
  export PGPORT=5432
  export PGUSER="$(id -un)"
  export PGDATABASE_NATIVE_QUERY=native_query
  export PGDATABASE_NATIVE_IVM=native_ivm
  "$postgres_prefix/bin/createdb" "$PGDATABASE_NATIVE_QUERY"
  "$postgres_prefix/bin/createdb" "$PGDATABASE_NATIVE_IVM"
fi

cd "$lab_dir"
node 6_runner.mjs "${runner_args[@]}"
printf 'wrote %s\n' "$output"
