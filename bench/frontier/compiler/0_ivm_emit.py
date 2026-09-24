"""Shared parsing of `dl8 emit sqlite` artifacts.

The emit artifact is `{"views": [{"relation", "stratum", "ddl"}], "diagnostics": [...]}`.
Each `ddl` is one `CREATE VIRTUAL TABLE "<name>" USING sqlite_ivm('<query>')`
statement whose module argument is the whole lowered program for one derived
relation: an optional `WITH [RECURSIVE] cte AS (body), ... ` prefix and a
tagged-union outer SELECT over per-product arms.
"""

import json
import re

DDL_RE = re.compile(
    r"^CREATE VIRTUAL TABLE (?P<name>\"(?:[^\"]|\"\")*\") USING sqlite_ivm\('(?P<query>.*)'\)$",
    re.DOTALL,
)


def load_emit(path):
    with open(path) as handle:
        return json.load(handle)


def split_top_level(text, separator):
    """Split on `separator` at parenthesis depth 0, respecting quotes."""
    parts, depth, start, i = [], 0, 0, 0
    while i < len(text):
        char = text[i]
        if char == "'":
            i += 1
            while i < len(text):
                if text[i] == "'":
                    if i + 1 < len(text) and text[i + 1] == "'":
                        i += 2
                        continue
                    break
                i += 1
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif depth == 0 and text.startswith(separator, i):
            parts.append(text[start:i])
            i += len(separator)
            start = i
            continue
        i += 1
    parts.append(text[start:])
    return parts


def unescape_text(text):
    return text.replace("''", "'")


def parse_ddl(ddl):
    """One DDL statement -> (view name, query)."""
    match = DDL_RE.match(ddl.strip().rstrip(";"))
    if not match:
        raise ValueError(f"unrecognized DDL shape: {ddl[:120]}...")
    return match.group("name"), unescape_text(match.group("query"))
def parse_query(query):
    """One module-arg query -> dict with the WITH prefix, CTE bodies, arms.

    Emitted shape (sprefa `src/_5_reify/_7_sqlite.rs`): an optional
    `WITH [RECURSIVE] <ctes> ` prefix and `SELECT <cols> FROM <target>`.
    The current emitter's outer target is always one quoted CTE name; a
    parenthesized UNION target would make the outer split ambiguous, so it
    is rejected instead of guessed. Rule bodies are the top-level UNION
    branches inside each CTE body.
    """
    with_clause = None
    rest = query
    match = re.match(r"^WITH (RECURSIVE )?", rest)
    if match:
        with_clause = "WITH RECURSIVE " if match.group(1) else "WITH "
        rest = rest[match.end():]

    def balanced_close(text, start):
        """Index one past the ')' closing the '(' at start-1."""
        depth = 1
        i = start
        while i < len(text) and depth:
            char = text[i]
            if char == "'":
                i += 1
                while i < len(text):
                    if text[i] == "'":
                        if i + 1 < len(text) and text[i + 1] == "'":
                            i += 2
                            continue
                        break
                    i += 1
            elif char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
            i += 1
        return i

    ctes = []
    pos = 0
    head = re.compile(
        r"\s*(?P<name>\"(?:[^\"]|\"\")+\")\s*\((?P<cols>[^()]*)\) AS \("
    )
    while True:
        found = head.match(rest, pos)
        if not found:
            break
        end = balanced_close(rest, found.end())
        ctes.append({
            "name": found.group("name"),
            "body": rest[found.end() : end - 1],
        })
        pos = end
        if pos < len(rest) and rest[pos] == ",":
            pos += 1
        else:
            break
    if match and not ctes:
        raise ValueError(f"no CTEs parsed in: {query[:120]}...")
    body = rest[pos:].lstrip()
    outer = re.match(r"^SELECT ", body)
    if not outer:
        raise ValueError(f"unrecognized outer SELECT: {body[:120]}...")
    # else (extra FROMs, parenthesized targets) is not an emitted shape.
    pieces = split_top_level(body, " FROM ")
    if len(pieces) != 2 or not pieces[0].startswith("SELECT "):
        raise ValueError(f"unrecognized outer SELECT: {body[:120]}...")
    outer_columns = pieces[0][len("SELECT "):]
    target = pieces[1].strip()
    if target.startswith("("):
        raise ValueError(
            "parenthesized UNION outer target not produced by the current "
            f"emitter: {target[:80]}..."
        )
    arms = [target]
    return {
        "recursive": with_clause == "WITH RECURSIVE ",
        "ctes": ctes,
        "arms": arms,
        "outer_columns": outer_columns,
    }


def referenced_relations(sql):
    """Table/CTE names a SQL piece reads: FROM and JOIN targets. The
    quoted-identifier shape matches what the lowering emits."""
    names = set()
    for match in re.finditer(
        r"\b(?:FROM|JOIN)\s+(?!SELECT\b)(\"(?:[^\"]|\"\")*\"|[A-Za-z_][A-Za-z0-9_.]*)",
        sql,
    ):
        names.add(match.group(1))
    return names
