"""SQL inventory for `dl8 emit sqlite` artifacts.

Usage: python3 2_inventory.py <outdir>

Reads every `<stem>.sql` in the directory plus `<stem>.json` (the compile
output, for program-level counts) and writes `<stem>.inventory.json` per case
plus a combined `inventory.tsv`. Metrics, all derived from the emitted text:

- `view_installs`    CREATE VIRTUAL TABLE ... USING sqlite_ivm statements
- `derived_heads`    outer UNION arms over the per-view tagged union, i.e. the
                     derived products the SQL materializes
- `rule_bodies`      CTE bodies inside the WITH prefix (one per compiled rule)
- `repeated_ctes`    CTE bodies whose full text appears in more than one view
- `dep_depth`        longest FROM/JOIN reference chain over the per-view CTE
                     name graph (1 = a CTE over base tables only)
- program-level: `program_rules` and `program_derived` straight from the
  compile output's runtime program, for the emitted-vs-programmed delta.
"""

import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib
_emit = importlib.import_module("0_ivm_emit")
load_emit, parse_ddl, parse_query = _emit.load_emit, _emit.parse_ddl, _emit.parse_query
referenced_relations, split_top_level = _emit.referenced_relations, _emit.split_top_level


def longest_chain(names, refs):
    depth = {}
    def walk(name, seen):
        if name in depth:
            return depth[name]
        if name in seen:
            return 0  # recursion: the chain closes on itself
        best = 1
        for target in sorted(refs.get(name, ())):
            if target in names:
                best = max(best, 1 + walk(target, seen | {name}))
        depth[name] = best
        return best
    return max((walk(n, frozenset()) for n in names), default=0)


def inventory(stem, out):
    sql = open(os.path.join(out, f"{stem}.sql")).read()
    program = json.load(open(os.path.join(out, f"{stem}.json")))

    statements = [s.strip() for s in sql.split(";\n") if s.strip()]
    views = []
    for statement in statements:
        name, query = parse_ddl(statement)
        parsed = parse_query(query)
        views.append((name, parsed))

    cte_bodies = [cte["body"] for _, parsed in views for cte in parsed["ctes"]]
    rule_bodies = []
    for body in cte_bodies:
        rule_bodies.extend(split_top_level(body, " UNION "))
    body_counts = {}
    for body in rule_bodies:
        body_counts[body] = body_counts.get(body, 0) + 1

    arms = sum(len(parsed["arms"]) for _, parsed in views)

    depth_best = 0
    for _, parsed in views:
        names = {cte["name"] for cte in parsed["ctes"]}
        refs = {cte["name"]: referenced_relations(cte["body"]) for cte in parsed["ctes"]}
        depth_best = max(depth_best, longest_chain(names, refs))

    # Program level: the compile output's rule rows (prelude included).
    rules = (program.get("program") or {}).get("rules", [])

    result = {
        "view_installs": len(views),
        "derived_heads": arms,
        "rule_bodies": len(rule_bodies),
        "repeated_ctes": sum(1 for count in body_counts.values() if count > 1),
        "dep_depth": depth_best,
        "program_rules": len(rules),
        "sql_bytes": len(sql),
        "views": [
            {
                "name": name,
                "recursive": parsed["recursive"],
                "ctes": [cte["name"] for cte in parsed["ctes"]],
            }
            for name, parsed in views
        ],
    }
    with open(os.path.join(out, f"{stem}.inventory.json"), "w") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    return result


def main():
    out = sys.argv[1]
    print("case\tview_installs\tderived_heads\trule_bodies\trepeated_ctes\tdep_depth\tprogram_rules\tsql_bytes")
    for path in sorted(os.listdir(out)):
        if not path.endswith(".sql"):
            continue
        stem = path[: -len(".sql")]
        row = inventory(stem, out)
        print(
            f"{stem}\t{row['view_installs']}\t{row['derived_heads']}\t"
            f"{row['rule_bodies']}\t{row['repeated_ctes']}\t{row['dep_depth']}\t"
            f"{row['program_rules']}\t{row['sql_bytes']}"
        )


if __name__ == "__main__":
    main()
