#!/usr/bin/env bash
# One command, one table: every tracing layer and every flush strategy in
# hafley-observe, priced as the difference between the same binary with the
# layer on and with it off.
#
# WATCH_BUDGET_SECONDS bounds the run. Past the bound the driver stops starting
# work, the table prints what it measured, and it names every cell it skipped.
# Re-running appends to the same raw file and resumes the skipped cells.
set -uo pipefail

crate_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
root=$(cd "$crate_dir/../.." && pwd)
lane_target=${CARGO_TARGET_DIR:-$root/target}
target=$lane_target/watch-the-watchman
logs=$target/logs
raw=$logs/raw.tsv
meta=$logs/meta.tsv
done_dir=$logs/done
budget=${WATCH_BUDGET_SECONDS:-600}
deadline=$(( $(date +%s) + budget ))
runs=${WATCH_RUNS:-3}

mkdir -p "$logs" "$done_dir"

# The endpoint names a collector that is not running, so an OTLP row prices the
# layer and a failing exporter, not a collector.
export HAFLEY_OTLP_ENDPOINT=${HAFLEY_OTLP_ENDPOINT:-http://127.0.0.1:4318/v1/traces}
export HAFLEY_TRACE=$logs/chrome.json

candidates=(
  'fmt||fmt|none'
  'chrome||chrome|none'
  'otlp-trace||otlp-trace|none'
  'otlp-metrics|otlp-trace|otlp-trace,otlp-metrics|none'
  'sysmetrics|otlp-metrics|otlp-metrics,sysmetrics|none'
  'procmetrics|otlp-metrics|otlp-metrics,procmetrics|none'
  'metrics-ctx|otlp-metrics|otlp-metrics,metrics-ctx|none'
  'tracy||tracy|none'
  'tracy-alloc||tracy-alloc|none'
  'rusage||rusage|none'
  'sqlite-sink||sqlite-sink|dictionary'
  'sqlite-sink-text||sqlite-sink|text'
)
strategies=(immediate drain on-commit)

skipped=()
stamp() { date +%s; }
clock() { python3 -c 'import time; print(f"{time.time():.3f}")'; }
since() { python3 -c "print(f'{float($2) - float($1):.3f}')"; }
# Progress goes to stderr: stdout is the table, and a command substitution that
# captures a long step must not capture the line announcing it.
progress() { printf '[%s/%ss] %s\n' "$(( $(date +%s) - deadline + budget ))" "$budget" "$1" >&2; }
out_of_time() { [ "$(date +%s)" -ge "$deadline" ]; }

build() {
  local features=$1
  # The package selector is required: without it cargo builds every workspace
  # member, and a sibling's default features would unify into this crate.
  local args=(-p hafley-observe --release --bin watch-the-watchman --target-dir "$target" --no-default-features)
  if [ -n "$features" ]; then
    args+=(--features "$features")
  fi
  cargo build "${args[@]}"
}

# The release artifact with its symbol table removed, which is the size a
# deployed binary has.
stripped_bytes() {
  local stripped=$logs/$(basename "$1").stripped
  if ! strip -S -x -o "$stripped" "$1" 2>/dev/null; then
    cp "$1" "$stripped"
  fi
  wc -c <"$stripped" | tr -d ' '
}

# Dependency nodes cargo resolves for a feature set, the harness binary and its
# dependencies included.
nodes() {
  local features=$1
  local args=(-e normal -p hafley-observe --no-default-features)
  if [ -n "$features" ]; then
    args+=(--features "$features")
  fi
  cargo tree "${args[@]}" 2>/dev/null | wc -l | tr -d ' '
}

# Three rebuilds of the crate and its binary with the dependency graph already
# built by the same command. A cold dependency build is not in this column.
rebuild_secs() {
  local candidate=$1 side=$2 features=$3
  local times=()
  local pass
  for pass in 1 2 3; do
    progress "rebuild $candidate/$side $pass of 3"
    touch "$crate_dir/src/lib.rs"
    local started
    started=$(clock)
    # Cargo's own progress stays on the terminal and in the log, so a long
    # rebuild prints as it goes.
    build "$features" >"$logs/build.$candidate.$side.log" 2> >(tee -a "$logs/build.$candidate.$side.log" >&2)
    times+=("$(since "$started" "$(clock)")")
  done
  printf '%s\n' "${times[*]}"
}

side_build() {
  local candidate=$1 side=$2 features=$3
  if [ -f "$logs/done/$candidate.$side.build" ]; then
    return 0
  fi
  progress "build $candidate/$side (features: ${features:-none})"
  if ! build "$features" >"$logs/build.$candidate.$side.log" 2> >(tee -a "$logs/build.$candidate.$side.log" >&2); then
    printf 'BUILD FAILED: %s %s (features: %s) see %s\n' "$candidate" "$side" "$features" "$logs/build.$candidate.$side.log"
    return 1
  fi
  local added bytes secs
  if [ "$side" = off ]; then
    added=$(nodes "$features")
    echo "$added" >"$logs/done/$candidate.base_nodes"
  fi
  local base
  base=$(cat "$logs/done/$candidate.base_nodes")
  added=$(nodes "$features")
  bytes=$(stripped_bytes "$target/release/watch-the-watchman")
  secs=$(rebuild_secs "$candidate" "$side" "$features")
  printf '%s\t%s\t%s\t%s\t%s\n' \
    "$candidate" "$side" "$(( added - base ))" "$bytes" "${secs// /,}" >>"$meta"
  touch "$logs/done/$candidate.$side.build"
}

run_once() {
  local candidate=$1 side=$2 run=$3 strategy=$4 sink=$5 off_features=$6
  local key="$candidate.$side.$strategy.$run"
  if [ -f "$done_dir/$key" ]; then
    return 0
  fi
  local db=$logs/log.$candidate.$strategy.$run.db
  local line
  line=$("$target/release/watch-the-watchman" \
    --feature "$candidate" --strategy "$strategy" --sink "$sink" --db "$db" 2>>"$logs/run.$candidate.log")
  if [ -z "$line" ]; then
    printf 'RUN FAILED: %s %s %s %s\n' "$candidate" "$side" "$strategy" "$run"
    return 1
  fi
  # Rail: a candidate whose off side is the empty feature set must compile with
  # no layers at all. A differential whose off side names a layer is not a
  # differential.
  local layers
  layers=$(printf '%s' "$line" | cut -f3)
  if [ "$side" = off ] && [ -z "$off_features" ] && [ "$layers" != none ]; then
    printf 'OFF SIDE CARRIES LAYERS: %s names %s\n' "$candidate" "$layers"
    return 1
  fi
  printf '%s\t%s\t%s\t%s\n' "$candidate" "$side" "$run" "$line" >>"$raw"
  touch "$done_dir/$key"
}

for entry in "${candidates[@]}"; do
  IFS='|' read -r candidate off on sink <<<"$entry"
  for side in off on; do
    features=$off
    [ "$side" = on ] && features=$on
    if out_of_time; then
      skipped+=("$candidate/$side (build)")
      continue
    fi
    side_build "$candidate" "$side" "$features" || skipped+=("$candidate/$side (build)")
    for strategy in "${strategies[@]}"; do
      progress "run $candidate/$side/$strategy x$runs"
      for run in $(seq 1 "$runs"); do
        if out_of_time; then
          skipped+=("$candidate/$side/$strategy/$run")
          continue
        fi
        run_once "$candidate" "$side" "$run" "$strategy" "$sink" "$off" ||
          skipped+=("$candidate/$side/$strategy/$run (failed)")
      done
    done
  done
  progress "measured $candidate"
done

progress "report"
python3 "$crate_dir/bench/watch_the_watchman_report.py" \
  --raw "$raw" --meta "$meta" \
  --tsv "$crate_dir/PLANS/watch-the-watchman.tsv"

if [ "${#skipped[@]}" -gt 0 ]; then
  printf '\nskipped %d cells:\n' "${#skipped[@]}"
  printf '  %s\n' "${skipped[@]}"
  printf 're-run with a larger WATCH_BUDGET_SECONDS to fill them; measured cells are kept.\n'
fi