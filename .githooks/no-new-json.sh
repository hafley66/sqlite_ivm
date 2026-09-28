#!/bin/sh
# Rejects a commit whose added source lines bring in SQLite json_* functions.
# Lines already in the tree pass.
pattern='\bjson_(array|object|extract|each|tree|group_array|group_object|insert|set|replace|patch|remove|type|valid|quote|array_length)\s*\('
hits=$(git diff --cached -U0 --diff-filter=AM -- '*.rs' '*.sql' '*.ts' '*.tsx' '*.js' '*.py' '*.dl7' '*.sh' \
  | awk '/^\+\+\+ b\//{file=substr($0,7); next} /^\+/{print file ": " substr($0,2)}' \
  | rg -i "$pattern")
if [ -n "$hits" ]; then
  cat >&2 <<'MSG'
STOP. NO NEW JSON. This commit adds SQLite json_* functions.

If you are an agent: stop this task now. Do not bypass with `git commit -n`,
do not rename or hide the JSON, do not try another encoding on your own.
Report the lines below to your parent or the human and ask for approval.
The human decides. Wait for the answer.

Lines:
MSG
  echo "$hits" >&2
  exit 1
fi
