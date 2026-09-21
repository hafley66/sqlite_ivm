#!/usr/bin/env bash
set -euo pipefail
# One recipe prints the statement counts. It runs tests/18_statement_counts.rs
# three times under the statement spans, then the wall pass in two builds: the
# statements build and the spans-compiled-out build. The tsv lands in
# plans/costs/every-statement.tsv; the markdown table prints on stdout.
#
# Counts and per-statement microseconds are the statements build's (SQLite's own
# profile clock, not our spans). Wall milliseconds are logging-off runs, and
# the recipe says which build each came from.
#
# usage: scripts/every-statement.sh
ivm_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out="$ivm_dir/plans/costs"
mkdir -p "$out"
runs=3
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

statement_run() {
  cargo test --offline --locked --manifest-path "$ivm_dir/Cargo.toml" \
    --test 18_statement_counts -- --nocapture \
    | awk -F'\t' 'NF>=12'
}
wall_run() {
  EVERY_STATEMENT_WALL=1 cargo test --offline --locked --manifest-path "$ivm_dir/Cargo.toml" "$@" \
    --test 18_statement_counts -- --nocapture \
    | grep -a '^WALL'
}

for i in $(seq 1 "$runs"); do
  statement_run > "$work/statement.$i.tsv"
done
wall_run > "$work/wall.statement.tsv"
wall_run --no-default-features --features bundled > "$work/wall.nostatement.tsv"

python3 - "$out/every-statement.tsv" "$work"/statement.*.tsv "$work"/wall.statement.tsv "$work"/wall.nostatement.tsv <<'PY'
import statistics
import sys

# Statement time on this SQLite build is quantized to one millisecond, so a
# cell whose median total falls below this floor reads as no effect at this
# scale. The denominator of the variance test is the median over three runs.
FLOOR_US = 1000.0
HEADER = [
    "scenario", "phase", "verb", "site", "object", "calls", "total_ms",
    "mean_us", "p99_us", "rows_touched", "prepared_pct", "per_input_row",
    "runs", "spread_us", "floor_us",
]

out_path = sys.argv[1]
statement_paths = sys.argv[2:-2]
wall_statement, wall_nostatement = sys.argv[-2], sys.argv[-1]

runs = []
for path in statement_paths:
    rows = {}
    with open(path) as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 12:
                continue
            key = tuple(parts[:5])
            rows[key] = {
                "calls": int(parts[5]),
                "total_us": float(parts[6]),
                "mean_us": float(parts[7]),
                "p99_us": float(parts[8]),
                # Blank when the statement site does not know its row count.
                "rows": parts[9],
                "prepared_pct": float(parts[10]),
                "per_input_row": float(parts[11]),
            }
    runs.append(rows)
if not runs or not runs[0]:
    sys.exit("statement produced no rows")

keys = set(runs[0])
for other in runs[1:]:
    if keys != set(other):
        sys.exit(f"statement groups differ across runs: missing={keys - set(other)}, added={set(other) - keys}")
    for key in keys:
        if (runs[0][key]["calls"], runs[0][key]["rows"]) != (
            other[key]["calls"],
            other[key]["rows"],
        ):
            sys.exit(f"statement counts differ across runs for {key}")

records = []
for key in sorted(keys):
    cells = [run[key] for run in runs]
    total = statistics.median(cell["total_us"] for cell in cells)
    spread = max(cell["total_us"] for cell in cells) - min(cell["total_us"] for cell in cells)
    scenario, phase, verb, site, obj = key
    total_ms = (
        "no effect at this scale" if total < FLOOR_US else f"{total / 1000.0:.3f}"
    )
    records.append([
        scenario, phase, verb, site, obj, str(cells[0]["calls"]), total_ms,
        f"{statistics.median(c['mean_us'] for c in cells):.1f}",
        f"{statistics.median(c['p99_us'] for c in cells):.1f}",
        cells[0]["rows"],
        f"{cells[0]['prepared_pct']:.1f}",
        f"{cells[0]['per_input_row']:.2f}",
        str(len(runs)),
        f"{spread:.1f}",
        f"{FLOOR_US:.0f}",
    ])

records.sort(key=lambda row: (row[0], -float(row[11])))
with open(out_path, "w") as handle:
    handle.write("\t".join(HEADER) + "\n")
    for record in records:
        handle.write("\t".join(record) + "\n")


def print_table(scenario):
    print(f"\n## {scenario}\n")
    print("| phase | verb | site | object | calls | total_ms | mean_us | p99_us | rows | prepared_pct | per_input_row | spread_us |")
    print("|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for row in records:
        if row[0] != scenario:
            continue
        print("| " + " | ".join(row[1:12]) + f" | {row[13]} |")


for scenario in dict.fromkeys(row[0] for row in records):
    print_table(scenario)


def wall(path):
    with open(path) as handle:
        return {
            parts[1]: tuple(float(value) for value in parts[2:5])
            for parts in (line.rstrip("\n").split("\t") for line in handle)
            if len(parts) == 5
        }


statement = wall(wall_statement)
compiled_out = wall(wall_nostatement)
print("\n## wall, logging off, median of three (min..max)\n")
print("| scenario | statements build ms | spans compiled out ms |")
print("|---|---:|---:|")
if not statement or statement.keys() != compiled_out.keys():
    sys.exit("wall scenario sets differ or are empty")
for name in statement:
    print(
        f"| {name} | {statement[name][1]:.3f} ({statement[name][0]:.3f}..{statement[name][2]:.3f}) "
        f"| {compiled_out[name][1]:.3f} ({compiled_out[name][0]:.3f}..{compiled_out[name][2]:.3f}) |"
    )
print(f"\nwrote {out_path}")
PY