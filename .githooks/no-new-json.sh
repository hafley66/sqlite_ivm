#!/bin/sh
# Rejects a commit whose added source lines bring in JSON: SQLite json_*
# functions or serde_json serialization. Lines already in the tree pass.
pattern='\bjson_(array|object|extract|each|tree|group_array|group_object|insert|set|replace|patch|remove|type|valid|quote|array_length)\s*\(|serde_json::(to_string|to_vec|to_value|to_writer)'
hits=$(git diff --cached -U0 --diff-filter=AM -- '*.rs' '*.sql' '*.ts' '*.tsx' '*.js' '*.py' '*.dl7' '*.sh' \
  | awk '/^\+\+\+ b\//{file=substr($0,7); next} /^\+/{print file ": " substr($0,2)}' \
  | rg -i "$pattern")
if [ -n "$hits" ]; then
  echo "no-new-json: added lines bring in JSON (git commit -n bypasses)" >&2
  echo "$hits" >&2
  exit 1
fi
