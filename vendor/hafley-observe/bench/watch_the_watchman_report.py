#!/usr/bin/env python3
"""Turn the harness's raw runs into the pricing table.

Every cell is a difference between the same binary with a candidate layer on
and with it off. A candidate is a cost only when all three on-runs sit outside
the spread of all three off-runs; anything else reads `in the noise` and still
prints the six numbers that say so.
"""

import argparse
import statistics

STRATEGIES = ("immediate", "drain", "on-commit")

NOTES = {
    "fmt": "formatter to a null writer, so formatting is priced and terminal I/O is not",
    "chrome": "timeline file under the lane target dir; the layer writes JSON from its own thread",
    "otlp-trace": "no collector is listening, so the exporter's batches fail; the row prices the layer plus a failing exporter",
    "otlp-metrics": "same failing exporter on the metrics signal path",
    "sysmetrics": "the bought observer runs on its own thread and samples on an interval, so most of its work is off the workload's wall; the row reads the noise it adds",
    "procmetrics": "the bought process collector through the metrics facade, sampled on a bounded span cadence",
    "metrics-ctx": "the bought span-field label layer over the facade recorder",
    "tracy": "tracy drops a span entered and exited on different threads, so its timeline is wrong under async",
    "tracy-alloc": "the tracked global allocator from the same client the span layer uses; its cost follows the allocation count, and this workload allocates little",
    "rusage": "the sampler is compiled into every build, so this row prices the publishing layer alone",
    "sqlite-sink": "dictionary-encoded: repeated columns interned once, per-row values stored as they are",
    "sqlite-sink-text": "the same shape with every key inlined; the R4 control for the dictionary",
}


def load(path):
    rows = []
    try:
        with open(path) as handle:
            for line in handle:
                cells = line.rstrip("\n").split("\t")
                if len(cells) < 16:
                    continue
                rows.append(
                    {
                        "candidate": cells[0],
                        "side": cells[1],
                        "run": int(cells[2]),
                        "feature": cells[3],
                        "strategy": cells[4],
                        "layers": cells[5],
                        "sink": cells[6],
                        "wall_ms": float(cells[7]),
                        "peak_rss": int(cells[8]),
                        "disk_read": int(cells[9]),
                        "disk_write": int(cells[10]),
                        "events": int(cells[11]),
                        "events_per_sec": float(cells[12]),
                        "rows": int(cells[13]),
                        "db_bytes": int(cells[14]),
                        "ns_per_event": float(cells[15]),
                    }
                )
    except FileNotFoundError:
        pass
    return rows


def load_meta(path):
    meta = {}
    try:
        with open(path) as handle:
            for line in handle:
                cells = line.rstrip("\n").split("\t")
                if len(cells) < 5:
                    continue
                meta.setdefault(cells[0], {})[cells[1]] = {
                    "crates_added": int(cells[2]),
                    "binary_bytes": int(cells[3]),
                    "build_secs": [float(value) for value in cells[4].split(",")],
                }
    except FileNotFoundError:
        pass
    return meta


def numbers(rows):
    return [row["wall_ms"] for row in sorted(rows, key=lambda row: row["run"])]


def median(rows, key):
    values = [row[key] for row in rows]
    return statistics.median(values) if values else None


def delta(on, off):
    if on is None or off is None:
        return "n/a"
    return f"{int(round(on - off)):+d}"


def verdict(off, on):
    if len(off) < 3 or len(on) < 3:
        return "in the noise"
    if min(on) > max(off):
        return "cost"
    if max(on) < min(off):
        return "gain"
    return "in the noise"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw", required=True)
    parser.add_argument("--meta", required=True)
    parser.add_argument("--tsv", required=True)
    args = parser.parse_args()

    rows = load(args.raw)
    meta = load_meta(args.meta)

    candidates = []
    for row in rows:
        if row["candidate"] not in candidates:
            candidates.append(row["candidate"])

    headers = [
        "feature",
        "strategy",
        "crates_added",
        "binary_bytes",
        "build_secs_off",
        "build_secs_on",
        "peak_rss_bytes",
        "disk_write_bytes",
        "disk_read_bytes",
        "wall_ms_off",
        "wall_ms_on",
        "pct_cost",
        "events_per_sec",
        "verdict",
        "notes",
    ]
    table = []
    for candidate in candidates:
        sides = meta.get(candidate, {})
        off_meta, on_meta = sides.get("off"), sides.get("on")
        for strategy in STRATEGIES:
            off = [row for row in rows if row["candidate"] == candidate and row["side"] == "off" and row["strategy"] == strategy]
            on = [row for row in rows if row["candidate"] == candidate and row["side"] == "on" and row["strategy"] == strategy]
            off_walls, on_walls = numbers(off), numbers(on)
            if not off_walls or not on_walls:
                table.append(
                    {
                        "feature": candidate,
                        "strategy": strategy,
                        "cells": {header: "skipped" for header in headers},
                    }
                )
                continue
            pct = statistics.median(on_walls) / statistics.median(off_walls) - 1.0
            table.append(
                {
                    "feature": candidate,
                    "strategy": strategy,
                    "cells": {
                        "feature": candidate,
                        "strategy": strategy,
                        "crates_added": str(on_meta["crates_added"]) if on_meta else "n/a",
                        "binary_bytes": delta(
                            on_meta["binary_bytes"] if on_meta else None,
                            off_meta["binary_bytes"] if off_meta else None,
                        ),
                        "build_secs_off": ",".join(
                            f"{value:.2f}" for value in (off_meta["build_secs"] if off_meta else [])
                        )
                        or "n/a",
                        "build_secs_on": ",".join(
                            f"{value:.2f}" for value in (on_meta["build_secs"] if on_meta else [])
                        )
                        or "n/a",
                        "peak_rss_bytes": delta(median(on, "peak_rss"), median(off, "peak_rss")),
                        "disk_write_bytes": delta(median(on, "disk_write"), median(off, "disk_write")),
                        "disk_read_bytes": delta(median(on, "disk_read"), median(off, "disk_read")),
                        "wall_ms_off": ",".join(f"{value:.2f}" for value in off_walls),
                        "wall_ms_on": ",".join(f"{value:.2f}" for value in on_walls),
                        "pct_cost": f"{pct:+.3f}",
                        "events_per_sec": f"{statistics.median([row['events_per_sec'] for row in on]):.0f}",
                        "verdict": verdict(off_walls, on_walls),
                        "notes": NOTES.get(candidate, ""),
                    },
                }
            )

    with open(args.tsv, "w") as handle:
        handle.write("\t".join(headers) + "\n")
        for row in table:
            handle.write("\t".join(row["cells"][header] for header in headers) + "\n")

    widths = {header: max(len(header), *(len(row["cells"][header]) for row in table)) if table else len(header) for header in headers}
    keep = ["feature", "strategy", "crates_added", "binary_bytes", "build_secs_on", "wall_ms_off", "wall_ms_on", "pct_cost", "events_per_sec", "verdict"]
    print("  ".join(header.ljust(widths[header]) for header in keep))
    for row in table:
        print("  ".join(row["cells"][header].ljust(widths[header]) for header in keep))
    print("\nnotes")
    for candidate in candidates:
        print(f"  {candidate}: {NOTES.get(candidate, '')}")
    print(f"\nfull table: {args.tsv}")


if __name__ == "__main__":
    main()