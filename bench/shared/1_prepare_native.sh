#!/usr/bin/env bash
set -euo pipefail

lab_dir=$(cd "$(dirname "$0")" && pwd)
work_dir=${IVM_WORK_DIR:-"$lab_dir/.work"}
postgres_version=18.6
pg_ivm_version=1.15
postgres_prefix="$work_dir/postgres-$postgres_version"
download_dir="$work_dir/downloads"
source_dir="$work_dir/src"
postgres_archive="$download_dir/postgresql-$postgres_version.tar.bz2"
postgres_source="$source_dir/postgresql-$postgres_version"
pg_ivm_source="$source_dir/pg_ivm-$pg_ivm_version"
postgres_shared="$postgres_prefix/share"

mkdir -p "$download_dir" "$source_dir"

if [[ ! -x "$postgres_prefix/bin/postgres" ]]; then
  if [[ ! -f "$postgres_archive" ]]; then
    curl --fail --location --output "$postgres_archive" \
      "https://ftp.postgresql.org/pub/source/v$postgres_version/postgresql-$postgres_version.tar.bz2"
    curl --fail --location --output "$postgres_archive.sha256" \
      "https://ftp.postgresql.org/pub/source/v$postgres_version/postgresql-$postgres_version.tar.bz2.sha256"
  fi
  (cd "$download_dir" && shasum -a 256 -c "postgresql-$postgres_version.tar.bz2.sha256")
  if [[ ! -d "$postgres_source" ]]; then
    tar -xjf "$postgres_archive" -C "$source_dir"
  fi
  if [[ ! -f "$postgres_source/GNUmakefile" ]]; then
    (cd "$postgres_source" && ./configure \
      --prefix="$postgres_prefix" \
      --without-icu \
      --without-readline \
      --without-zlib \
      --without-lz4 \
      --without-zstd)
  fi
  make -C "$postgres_source" -j "${IVM_MAKE_JOBS:-4}"
  make -C "$postgres_source" install
fi

if [[ -x "$postgres_prefix/bin/pg_config" ]]; then
  postgres_shared=$($postgres_prefix/bin/pg_config --sharedir)
fi

if [[ ! -f "$postgres_shared/extension/pg_ivm.control" ]]; then
  if [[ ! -d "$pg_ivm_source/.git" ]]; then
    git clone --depth 1 --branch "v$pg_ivm_version" \
      https://github.com/sraoss/pg_ivm.git "$pg_ivm_source"
  fi
  make -C "$pg_ivm_source" PG_CONFIG="$postgres_prefix/bin/pg_config"
  make -C "$pg_ivm_source" PG_CONFIG="$postgres_prefix/bin/pg_config" install
fi

postgres_reported=$($postgres_prefix/bin/postgres --version | sed 's/^postgres (PostgreSQL) //')
pg_ivm_reported=$(sed -n "s/^default_version = '\([^']*\)'.*/\1/p" \
  "$postgres_shared/extension/pg_ivm.control")
pg_ivm_commit=$(git -C "$pg_ivm_source" rev-parse HEAD)
printf '{"event":"native-dependencies-ready","postgres_version":"%s","pg_ivm_version":"%s","pg_ivm_commit":"%s","prefix":"%s"}\n' \
  "$postgres_reported" "$pg_ivm_reported" "$pg_ivm_commit" "$postgres_prefix"
