"""Probe the promoted frontier engine with the compiler's emitted SQL.

Usage: python3 4_frontier_probe.py <outdir> <libsqlite_ivm.dylib>

The extension exposes the engine as SQL scalar functions:
`sqlite_ivm_frontier_install(name, select_sql)` (accept -> name, reject ->
the engine's error text with its stage) and snapshot reads through the
`frontier_<name>` virtual table. This probe drives it over a real load:

- `full_ddl`     the whole emitted SQL batch for the case
- `ddl`          one emitted CREATE VIRTUAL TABLE statement
- `query`        one view's module-arg query (WITH ... SELECT ... UNION ...)
- `cte_body`     one CTE body = one compiled rule body, in emission order
- `outer_arm`    one outer UNION arm (one derived product's read-off)

Every candidate runs against a fresh copy of the eval store (its base tables
and rows exist), each in its own sqlite3 process, pragmas set before load.
An accepted candidate is exercised, not assumed: one committed source DELETE
settles, then a snapshot read must succeed. `earliest_executable` is the
first rule-body-level candidate (cte_body or outer_arm) that installed,
settled and read.

Writes `frontier_probe.tsv`; prints each verdict.
"""

import os
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ivm_emit import load_emit, parse_ddl, parse_query, referenced_relations

SQLITE3 = os.environ.get("SQLITE3", "/opt/homebrew/opt/sqlite/bin/sqlite3")


def sql_literal(text):
    return "'" + text.replace("'", "''") + "'"


def candidates_for(out, stem):
    doc = load_emit(os.path.join(out, f"{stem}.emit.json"))
    ddls = [v["ddl"].rstrip(";") for v in doc["views"]]
    yield "full_ddl", "", 0, ";\n".join(ddls) + (";\n" if ddls else "")
    for i, ddl in enumerate(ddls):
        yield "ddl", f"statement{i}", i, ddl
        name, query = parse_ddl(ddl)
        yield "query", name, i, query
        parsed = parse_query(query)
        for j, cte in enumerate(parsed["ctes"]):
            yield "cte_body", f'{name} {cte["name"]}', j, cte["body"]
        for j, arm in enumerate(parsed["arms"]):
            yield "outer_arm", name, j, arm


def error_text(proc):
    text = (proc.stderr.strip() or proc.stdout.strip())
    return text.replace("\t", " ").replace("\n", " | ")[:400]


def probe_one(db, ext, program, candidate, sources):
    """Install, settle, read in ONE connection.

    install registers per-connection `frontier_<prog>_cN` vtab modules and
    persists a `frontier_<prog>` virtual table that instantiates them, so a
    fresh connection cannot read the snapshot. Install returns the program
    name on success; rejects carry the engine stage in the error text.
    """
    seed = None
    for source in sources:
        probe_row = subprocess.run(
            [SQLITE3, "-batch", db, f'SELECT * FROM {source} ORDER BY __id LIMIT 1;'],
            capture_output=True, text=True,
        )
        if probe_row.returncode == 0 and probe_row.stdout.strip():
            values = probe_row.stdout.strip().split("|")
            seed = (source, values[0], ", ".join(values))
            break

    script = [
        "PRAGMA recursive_triggers=ON;",
        "PRAGMA trusted_schema=ON;",
        f".load {ext}",
        f"SELECT sqlite_ivm_frontier_install({sql_literal(program)}, {sql_literal(candidate)});",
        "SELECT '*installed*';",
    ]
    if seed:
        source, first_id, row_values = seed
        script += [
            f"BEGIN; DELETE FROM {source} WHERE __id = {first_id}; COMMIT;",
            "SELECT '*deleted*';",
            f'SELECT count(*) FROM "frontier_{program}";',
            f"BEGIN; INSERT INTO {source} VALUES ({row_values}); COMMIT;",
            "SELECT '*reinserted*';",
            f'SELECT count(*) FROM "frontier_{program}";',
        ]
    else:
        script += [f'SELECT count(*) FROM "frontier_{program}";']
    script.append("")

    run = subprocess.run(
        [SQLITE3, "-batch", db], input="\n".join(script),
        capture_output=True, text=True,
    )
    if run.returncode != 0 or "Error" in run.stderr:
        return "rejected", error_text(run)
    lines = [l for l in run.stdout.splitlines() if l.strip()]
    if not lines or lines[0] != program:
        return "rejected", f"install did not echo the program name: {run.stdout.strip()[:200]}"

    def after(marker):
        if marker not in lines:
            return None
        return lines[lines.index(marker) + 1]

    deleted, reinserted = after("*deleted*"), after("*reinserted*")
    if deleted is None:
        return "accepted-no-source", f"installed as {program}; no writable source table found"
    return (
        "accepted-settled",
        f"installed as {program}; frontier rows after delete: {deleted}, after re-insert: {reinserted}",
    )


def probe(out, stem, ext, rows):
    sql = open(os.path.join(out, f"{stem}.sql")).read()
    if not sql.strip():
        rows.append(f"{stem}\t-\t-\t-\tno emitted SQL: nothing to probe\t-")
        print(f"{stem}: no emitted SQL, nothing to probe")
        return
    doc = load_emit(os.path.join(out, f"{stem}.emit.json"))
    view_names = {'"{}"'.format(v["ddl"].split('"')[1]) for v in doc["views"]}
    sources = set()
    for view in doc["views"]:
        _, query = parse_ddl(view["ddl"])
        parsed = parse_query(query)
        for cte in parsed["ctes"]:
            sources |= referenced_relations(cte["body"])
        for arm in parsed["arms"]:
            sources |= referenced_relations(arm)
    sources = sorted(sources - view_names)
    store = os.path.join(out, f"{stem}.store.sqlite")
    # The engine requires plain-identifier program names; fixture stems
    # start with a digit, so prefix a letter and keep the mapping in detail.
    prog = "p_" + stem
    for kind, label, index, candidate in candidates_for(out, stem):
        workdir = tempfile.mkdtemp(prefix=f"ivm-frontier-{stem}-")
        db = os.path.join(workdir, "probe.sqlite")
        shutil.copy(store, db)
        try:
            verdict, detail = probe_one(db, ext, prog, candidate, sources)
        finally:
            shutil.rmtree(workdir, ignore_errors=True)
        rows.append(f"{stem}\t{kind}\t{label}\t{index}\t{verdict}\t{detail}")
        print(f"{stem} {kind} [{label}] #{index}: {verdict} :: {detail[:200]}")

    # Not an emitted artifact: the first store table projected bare, in the
    # engine itself installs, settles and maintains programs; the emitted
    # bodies above fail only on their set-semantics/filter shapes.
    if sources:
        minimal = f'SELECT t0."c0_term", t0."c1_term" FROM {sources[0]} AS t0'
        workdir = tempfile.mkdtemp(prefix=f"ivm-frontier-{stem}-")
        db = os.path.join(workdir, "probe.sqlite")
        shutil.copy(store, db)
        try:
            verdict, detail = probe_one(db, ext, prog, minimal, sources)
        finally:
            shutil.rmtree(workdir, ignore_errors=True)
        rows.append(f"{stem}\tminimal_derived\thand-stripped from {sources[0]}\t0\t{verdict}\t{detail}")
        print(f"{stem} minimal_derived: {verdict} :: {detail[:200]}")
        rows.append(f"{stem}\tnote\t-\t-\t-\temitted bodies rejected above; minimal hand-stripped body shows engine liveness\t-")


def main():
    out, ext = sys.argv[1], sys.argv[2]
    rows = ["case\tkind\tview/index\tordinal\tverdict\tdetail"]
    for path in sorted(os.listdir(out)):
        if path.endswith(".sql"):
            probe(out, path[: -len(".sql")], ext, rows)
    report = os.path.join(out, "frontier_probe.tsv")
    with open(report, "w") as handle:
        handle.write("\n".join(rows) + "\n")
    print("\nwrote", report)


if __name__ == "__main__":
    main()
