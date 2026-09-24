#!/usr/bin/env bash
# Compiler-to-SQL artifacts for the frontier compiler inventory.
#
# Usage: bash 1_compile_emit.sh <dl8> <sprefa-checkout> <outdir>
#
# Cases:
#   2_partial      tests/fixtures/2_partial.dl7      the fixed DL7 case
#   1_transitive   fixtures/sqlite_emit/1_transitive.dl7   nearest emitting case
#   0_union_filter fixtures/sqlite_emit/0_union_filter.dl7 nearest emitting case
#
# Per case: `dl8 compile` -> program JSON; `dl8 emit sqlite` -> view DDL JSON;
# the raw SQL artifact `<stem>.sql` (one CREATE VIRTUAL TABLE per line block);
# `dl8 eval --db` -> the persisted store + closure. Every exit code lands in
# `receipt.tsv` so a refused case stays a recorded result, not a crash.
#
# c2 oracle parity: every `oracle/eval/c2_partial_*.json` case program runs
# under the default (Rust) engine and under DL8_ENGINE=sqlite, and each run is
# compared against the frozen v7 closure. Results land in `c2_parity.tsv`.
set -euo pipefail

dl8=${1:?dl8 binary path}
sprefa=${2:?sprefa checkout root}
out=${3:?artifact output directory}
mkdir -p "$out"
: > "$out/receipt.tsv"

emit_case() {
  local stem=$1 src=$2
  local code
  "$dl8" compile "$sprefa/$src" > "$out/$stem.json" 2> "$out/$stem.compile.stderr.txt"
  printf 'compile\t%s\t%s\n' "$stem" "$?" >> "$out/receipt.tsv"
  code=0
  "$dl8" emit sqlite "$out/$stem.json" > "$out/$stem.emit.json" 2> "$out/$stem.emit.stderr.txt" || code=$?
  printf 'emit\t%s\t%s\n' "$stem" "$code" >> "$out/receipt.tsv"
  STEM=$stem OUT=$out python3 - <<'PY'
import json, os
out, stem = os.environ["OUT"], os.environ["STEM"]
doc = json.load(open(f"{out}/{stem}.emit.json"))
sql = "".join(v["ddl"].rstrip(";") + ";\n" for v in doc["views"])
open(f"{out}/{stem}.sql", "w").write(sql)
PY
  code=0
  "$dl8" eval --db "$out/$stem.store.sqlite" "$out/$stem.json" \
    > "$out/$stem.closure.json" 2> "$out/$stem.eval-db.stderr.txt" || code=$?
  printf 'eval-db\t%s\t%s\n' "$stem" "$code" >> "$out/receipt.tsv"
  code=0
  DL8_ENGINE=sqlite "$dl8" eval "$out/$stem.json" \
    > "$out/$stem.closure-sqlite.json" 2> "$out/$stem.eval-sqlite.stderr.txt" || code=$?
  printf 'eval-sqlite\t%s\t%s\n' "$stem" "$code" >> "$out/receipt.tsv"
}

emit_case 2_partial tests/fixtures/2_partial.dl7
emit_case 1_transitive fixtures/sqlite_emit/1_transitive.dl7
emit_case 0_union_filter fixtures/sqlite_emit/0_union_filter.dl7

# c2: the frozen eval-oracle cases of the 2_partial fixture.
: > "$out/c2_parity.tsv"
for case in "$sprefa"/oracle/eval/c2_partial_*.json; do
  name=$(basename "$case" .json)
  CASE="$case" NAME="$name" OUT="$out" DL8="$dl8" python3 - <<'PY'
import json, os, subprocess
case, name, out, dl8 = (
    os.environ["CASE"], os.environ["NAME"], os.environ["OUT"], os.environ["DL8"],
)
doc = json.load(open(case))
want = doc["expected"]
program = os.path.join(out, f"{name}.program.json")
open(program, "w").write(json.dumps(doc["program"]))

def run(engine):
    env = dict(os.environ)
    if engine:
        env["DL8_ENGINE"] = engine
    else:
        env.pop("DL8_ENGINE", None)
    got = subprocess.run([dl8, "eval", program], env=env, capture_output=True)
    try:
        parsed = json.loads(got.stdout)
    except Exception:
        parsed = None
    if parsed is None:
        return "no-json", []
    same = (
        parsed.get("closure") == want.get("closure")
        and parsed.get("diagnostics") == want.get("diagnostics")
    )
    shapes = [
        d["payload"]["args"][0]["a"]
        if isinstance(d.get("payload"), dict) and d["payload"].get("args")
        else json.dumps(d.get("payload"))[:40]
        for d in parsed.get("diagnostics", [])
    ]
    return ("match" if same else "differs"), shapes

for engine in ("rust", "sqlite"):
    verdict, shapes = run(engine if engine != "rust" else "")
    print(f"{name}\t{engine}\t{verdict}\t{';'.join(shapes[:4])}")
PY
done >> "$out/c2_parity.tsv"

cat "$out/receipt.tsv"
cat "$out/c2_parity.tsv"
