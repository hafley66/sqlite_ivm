#!/usr/bin/env python3
"""Record and check the gate's per-test walls from nextest's JUnit report.

record: three reports in, per-test median out, one row per platform in
scripts/timeout-rail.tsv.
check:  one report against the recorded row for the platform.

Level 3 of the timeout rail. A test regresses when its wall is above both
recorded*RATIO and recorded+ALLOWANCE. The two thresholds are measured, not
guessed: over three base runs the largest relative spread was 250 percent on a
0.010s leg and the largest absolute spread was 0.047s on a 0.101s leg, so a
ratio alone flaps on process-start jitter and an allowance alone is blind to a
small leg doubling. ALLOWANCE is 3.2x that largest spread. Both numbers, and
the raw runs, are in plans/costs/timeout-rail.md.
"""

import statistics
import sys
import xml.etree.ElementTree as ET

RATIO = 1.5
ALLOWANCE = 0.150
BATTERY = "BATTERY"


def read_report(path):
    root = ET.parse(path).getroot()
    walls = {}
    for case in root.iter("testcase"):
        wall = case.get("time")
        if wall is None:
            continue
        walls[f"{case.get('classname')}::{case.get('name')}"] = float(wall)
    return walls, float(root.get("time"))


def load(path):
    rows = {}
    for line in open(path):
        if line.startswith("#") or not line.strip():
            continue
        platform, test, seconds = line.rstrip("\n").split("\t")
        rows.setdefault(platform, {})[test] = float(seconds)
    return rows


def record(platform, out, reports):
    runs = [read_report(path) for path in reports]
    names = sorted(set().union(*(walls for walls, _ in runs)))
    lines = ["# platform\ttest\tseconds", "# written by scripts/timeout-rail-record.sh"]
    for name in names:
        walls = [walls[name] for walls, _ in runs if name in walls]
        lines.append(f"{platform}\t{name}\t{statistics.median(walls):.3f}")
    lines.append(f"{platform}\t{BATTERY}\t{statistics.median(t for _, t in runs):.3f}")
    with open(out, "w") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"recorded {len(names)} tests and the battery for {platform} into {out}")


def check(platform, tsv_path, report):
    try:
        recorded = load(tsv_path).get(platform)
    except FileNotFoundError:
        recorded = None
    if recorded is None:
        print(
            f"timeout rail: no recorded timings for {platform} in {tsv_path}; "
            "levels 1 and 2 still apply. Run scripts/timeout-rail-record.sh here "
            "and commit the row."
        )
        return 0
    walls, total = read_report(report)
    failed = []
    for name, wall in sorted(walls.items()):
        baseline = recorded.get(name)
        if baseline is None:
            print(f"timeout rail: no recorded number for {name} at {wall:.3f}s")
            continue
        if wall > baseline * RATIO and wall > baseline + ALLOWANCE:
            failed.append(
                f"{name} {wall:.3f}s recorded {baseline:.3f}s "
                f"(over {baseline * RATIO:.3f}s ratio and {baseline + ALLOWANCE:.3f}s absolute)"
            )
    baseline = recorded.get(BATTERY)
    if baseline is not None and total > baseline * RATIO and total > baseline + ALLOWANCE:
        failed.append(
            f"{BATTERY} {total:.3f}s recorded {baseline:.3f}s "
            f"(over {baseline * RATIO:.3f}s ratio and {baseline + ALLOWANCE:.3f}s absolute)"
        )
    if failed:
        for line in failed:
            print(f"timeout rail: regression {line}")
        print(
            f"timeout rail: {len(failed)} leg(s) above the recorded wall. Re-run "
            "scripts/timeout-rail-record.sh on a quiet machine only after the "
            "regression is understood."
        )
        return 1
    print(f"timeout rail: {len(walls)} tests and the battery within tolerance for {platform}")


def main():
    mode = sys.argv[1]
    if mode == "record":
        record(sys.argv[2], sys.argv[3], sys.argv[4:])
    elif mode == "check":
        return check(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())