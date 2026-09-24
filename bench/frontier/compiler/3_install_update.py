"""Install the emitted DDL over the eval store and measure it.

Usage: python3 3_install_update.py <outdir> <libsqlite_ivm.dylib> [reps]

Per case with emitted SQL, per rep (default 3, fresh copy of the eval store):

- install   every emitted CREATE VIRTUAL TABLE, one sqlite3 process,
            `PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;`
- read      every installed view's full contents, in artifact order
- update    DELETE the store's first source row inside a committed
            transaction (sqlite_ivm settles through its triggers), then
            re-INSERT the identical row; a read follows each update
- metrics   per-statement wall time from the CLI `.timer`, process peak RSS
            (/usr/bin/time -l), SQLite allocator current/peak (`.stats on`:
            the host allocator the extension runs inside), database + WAL
            bytes after `PRAGMA wal_checkpoint(TRUNCATE)`

Writes `install_update.tsv` and `<stem>.eqp.txt` (EXPLAIN QUERY PLAN of every
view query: the representative parsed clauses of the emitted SQL).
"""

import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib
_emit = importlib.import_module("0_ivm_emit")
load_emit, parse_ddl, parse_query = _emit.load_emit, _emit.parse_ddl, _emit.parse_query
referenced_relations, split_top_level = _emit.referenced_relations, _emit.split_top_level

SQLITE3 = os.environ.get("SQLITE3", "/opt/homebrew/opt/sqlite/bin/sqlite3")
TIMER_LINE = re.compile(r"^Run Time: real ([0-9.]+) .*$", re.MULTILINE)


def sql_query_of(ddl):
    """The module-arg query text of one CREATE VIRTUAL TABLE statement."""
    return ddl[ddl.index("('") + 2 : ddl.rindex("')")].replace("''", "'")

def run_cli(db, ext, script):
    """One sqlite3 process with -bail; returns (stdout, stderr, merged, rc, rss)."""
    wrapped = "\n".join([
        "PRAGMA recursive_triggers=ON;",
        "PRAGMA trusted_schema=ON;",
        f".load {ext}",
        script,
        "",
    ])
    with tempfile.NamedTemporaryFile("w", suffix=".sql", delete=False) as handle:
        handle.write(wrapped)
        path = handle.name
    try:
        timed = subprocess.run(
            ["/usr/bin/time", "-l", SQLITE3, "-batch", "-bail", db, f".read {path}"],
            capture_output=True, text=True,
        )
    finally:
        os.unlink(path)
    rss = 0
    match = re.search(r"^\s*(\d+)\s+maximum resident set size", timed.stderr, re.MULTILINE)
    if match:
        rss = int(match.group(1))
    merged = timed.stdout + "\n" + timed.stderr
    return timed.stdout, timed.stderr, merged, timed.returncode, rss


def statement_ms(merged):
    return [round(float(m) * 1000, 3) for m in TIMER_LINE.findall(merged)]

def read_block(views):
    lines = [".mode list", ".separator |", ".headers off"]
    for name, ddl in views:
        # ORDER BY every projected column; the emitted views have <= 4.
        query = parse_ddl(ddl)[1]
        columns = len(split_top_level(parse_query(query)["outer_columns"], ","))
        order = ", ".join(str(i) for i in range(1, min(columns, 4) + 1))
        lines.append(f"SELECT '*VIEW* {name}';")
        lines.append(f"SELECT * FROM {name} ORDER BY {order};")
    return "\n".join(lines)


def hash_text(text):
    return hashlib.sha256(text.encode()).hexdigest()[:16]

def views_of(out, stem):
    """(quoted view name, DDL) per emitted view, in emission order."""
    doc = load_emit(os.path.join(out, f"{stem}.emit.json"))
    return [('"{}"'.format(v["ddl"].split('"')[1]), v["ddl"]) for v in doc["views"]]


def source_tables(out, stem, views):
    """Store tables the emitted SQL reads, in first-reference order."""
    doc = load_emit(os.path.join(out, f"{stem}.emit.json"))
    refs = []
    for view in doc["views"]:
        _, query = parse_ddl(view["ddl"])
        parsed = parse_query(query)
        for cte in parsed["ctes"]:
            refs += sorted(referenced_relations(cte["body"]) - set(n for n, _ in views))
        for arm in parsed["arms"]:
            refs += sorted(referenced_relations(arm) - set(n for n, _ in views))
    seen, unique = set(), []
    for name in refs:
        if name not in seen:
            seen.add(name)
            unique.append(name)
    return unique


def eqp_file(out, stem, store, views):
    with open(os.path.join(out, f"{stem}.eqp.txt"), "w") as handle:
        for name, ddl in views:
            handle.write(f"-- {name}\n")
            proc = subprocess.run(
                [SQLITE3, "-batch", store, "EXPLAIN QUERY PLAN " + sql_query_of(ddl) + ";"],
                capture_output=True, text=True,
            )
            handle.write(proc.stdout)
            if proc.returncode != 0:
                handle.write(f"-- EQP ERROR: {proc.stderr.strip()}\n")


def case_run(out, stem, ext, reps, rows):
    sql = open(os.path.join(out, f"{stem}.sql")).read()
    if not sql.strip():
        rows.append(f"{stem}\t-\t-\tno emitted SQL: every rule refused at emit\t" + "\t".join(["-"] * 13))
        return
    views = views_of(out, stem)
    store = os.path.join(out, f"{stem}.store.sqlite")
    sources = source_tables(out, stem, views)
    eqp_file(out, stem, store, views)

    for rep in range(1, reps + 1):
        workdir = tempfile.mkdtemp(prefix=f"ivm-inv-{stem}-{rep}-")
        db = os.path.join(workdir, "probe.sqlite")
        shutil.copy(store, db)

        source, first_id, first_row = None, None, None
        for candidate in sources:
            got = subprocess.run(
                [SQLITE3, "-batch", db,
                 f'SELECT __id FROM {candidate} ORDER BY __id LIMIT 1;'],
                capture_output=True, text=True,
            )
            if got.returncode == 0 and got.stdout.strip():
                source = candidate
                first_id = got.stdout.strip()
                first_row = subprocess.run(
                    [SQLITE3, "-batch", db,
                     f'SELECT * FROM {source} WHERE __id = {first_id};'],
                    capture_output=True, text=True,
                ).stdout.strip()
                break

        # Typed round-trip: the seed row is saved into a TEMP table inside
        # the same connection and restored with SELECT *; no CLI text
        # reconstruction, so storage classes and values are SQLite's own.
        script = [".timer on", sql, read_block(views)]
        if source:
            script += [
                f"CREATE TEMP TABLE ivm_saved AS SELECT * FROM {source} WHERE __id = {first_id};",
                f"BEGIN; DELETE FROM {source} WHERE __id = {first_id}; COMMIT;",
                read_block(views),
                f"BEGIN; INSERT INTO {source} SELECT * FROM ivm_saved; COMMIT;",
                read_block(views),
            ]
        script += ["SELECT '*END*';", ".stats"]

        stdout, stderr, merged, rc, rss = run_cli(db, ext, "\n".join(script))
        if stderr.strip():
            print(f"{stem} rep{rep} stderr: {stderr.strip()[:400]}", file=sys.stderr)

        times = statement_ms(merged)
        blocks = stdout.split("*VIEW* ")[1:]

        numeric_row = re.compile(r"\d+(\|\d+)*")

        def data_lines(block):
            """(numeric rows, count of non-numeric non-timer rows)."""
            body = block.split("\n", 1)[1].split("*END*", 1)[0]
            rows_out, alien = [], 0
            for line in body.splitlines():
                if not line.strip() or line.startswith("Run Time:"):
                    continue
                if numeric_row.fullmatch(line):
                    rows_out.append(line)
                else:
                    alien += 1
            return rows_out, alien

        n = len(views)
        status = "ok"
        error_match = re.search(
            r"(?:Parse error|Error|Runtime error)[^\n]*", merged, re.MULTILINE
        )
        if rc != 0 or error_match:
            status = "failed: " + (error_match.group(0)[:80] if error_match else f"rc={rc}")

        phase, alien_total = [], 0
        reads = 3 if source else 1
        for read in range(reads):
            lines, alien = [], 0
            for view in range(n):
                got, bad = data_lines(blocks[read * n + view])
                lines += got
                alien += bad
            alien_total += alien
            phase.append(hash_text("\n".join(lines)) if not alien else f"nonnumeric({alien})")
        hash_install = phase[0]
        hash_delete = phase[1] if len(phase) > 1 else "-"
        hash_insert = phase[2] if len(phase) > 2 else "-"
        hash_valid = "yes" if alien_total == 0 and all(p != "-" for p in phase) else f"no({alien_total} non-numeric rows)"

        delete_ms = insert_ms = ""
        if source and status == "ok":
            # timer layout per view n: n installs, read1, temp-save,
            # delete+commit, read2, insert+commit, read3
            if len(times) >= n + 5:
                delete_ms = times[n + 2]
                insert_ms = times[n + 4]
        install_ms = round(sum(times[:n]), 3) if status == "ok" else ""


        mem = re.search(r"Memory Used:\s+(\d+) \(max (\d+)\)", merged)
        mem_current, mem_peak = (mem.group(1), mem.group(2)) if mem else ("", "")
        # WAL is read BEFORE any checkpoint so the number reflects real
        # post-commit WAL; the checkpoint then runs as its own process.
        db_bytes = os.path.getsize(db) if os.path.exists(db) else 0
        wal_bytes = os.path.getsize(db + "-wal") if os.path.exists(db + "-wal") else 0
        subprocess.run([SQLITE3, "-batch", db, "PRAGMA wal_checkpoint(TRUNCATE);"],
                       capture_output=True, text=True)
        rows.append(
            f"{stem}\t{rep}\t{rc}\t{status}\t{install_ms}\t{delete_ms}\t{insert_ms}\t"
            f"{hash_install}\t{hash_delete}\t{hash_insert}\t{hash_valid}\t{rss}\t"
            f"{mem_current}\t{mem_peak}\t{db_bytes}\t{wal_bytes}"
        )
        shutil.rmtree(workdir, ignore_errors=True)


def main():
    out, ext = sys.argv[1], sys.argv[2]
    reps = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    rows = [
        "case\trep\trc\tstatus\tinstall_ms\tdelete_ms\tinsert_ms\thash_install\t"
        "hash_after_delete\thash_after_insert\thash_valid\trss_bytes\t"
        "sqlite_mem_current\tsqlite_mem_peak\tdb_bytes\twal_bytes_post_commit"
    ]
    for path in sorted(os.listdir(out)):
        if path.endswith(".sql"):
            case_run(out, path[: -len(".sql")], ext, reps, rows)
    report = os.path.join(out, "install_update.tsv")
    with open(report, "w") as handle:
        handle.write("\n".join(rows) + "\n")
    print("\n".join(rows))


if __name__ == "__main__":
    main()
