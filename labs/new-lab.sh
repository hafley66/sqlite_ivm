#!/usr/bin/env bash
# Mint an ISO lab directory. Title card is the hypothesis.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
labs="$root/labs"

die() { printf 'new-lab: %s\n' "$1" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: new-lab.sh <the-gang-does-a-thing>"
title="$1"

case "$title" in
  the-gang-*) ;;
  *) die "title card must start with 'the-gang-'. It is always sunny in the lab." ;;
esac
[[ "$title" =~ ^[a-z0-9-]+$ ]] || die "title card is lowercase kebab only: [a-z0-9-]"
[[ "$title" != *--* ]] || die "no double dashes"
[[ "$title" != *- ]] || die "no trailing dash"
words=$(tr '-' '\n' <<<"$title" | grep -c .)
[ "$words" -ge 5 ] || die "title card needs a verb and an object, not just 'the-gang-x'"
[ "${#title}" -le 72 ] || die "title card over 72 chars, shorten the hypothesis"

stamp="$(date +%Y%m%d)"
index=0
while [ -e "$labs/$stamp.$index."* ] 2>/dev/null || compgen -G "$labs/$stamp.$index.*" >/dev/null; do
  index=$((index + 1))
done

dir="$labs/$stamp.$index.$title"
mkdir -p "$dir/src"

# ISO: no path dependency on the entry point. Shared crates pinned to the
# exact versions the entry point resolves, read from the root manifest.
pin() { grep -E "^$1 = " "$root/Cargo.toml" | head -1; }

{
  printf '[package]\nname = "lab_%s_%s"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n\n' "$stamp" "$index"
  printf '# Own workspace root. A lab never joins the entry point workspace.\n[workspace]\n\n'
  printf '[dependencies]\n'
  pin rusqlite
  pin tracing
  pin tracing-subscriber
  printf '\n[dev-dependencies]\nserde_json = "1"\n'
} > "$dir/Cargo.toml"

{
  printf '# %s\n\n' "$(tr '-' ' ' <<<"$title" | sed 's/\b\(.\)/\u\1/g')"
  printf '## Hypothesis\n\nThe title card is the hypothesis. Restate it as a falsifiable claim.\n\n'
  printf '## Knob\n\nWhich SQLite mechanism this lab turns.\n\n'
  printf '## Invariants assumed of the input tables\n\n- \n\n'
  printf '## Measurement\n\nCommand that prints the number. No number lives here until it ran.\n\n'
  printf '## Invalidates\n\nWhich other labs die if this one comes back positive or negative.\n\n'
  printf '## Verdict\n\nUnrun.\n'
} > "$dir/HYPOTHESIS.md"

printf 'fn main() {\n    todo!("%s")\n}\n' "$title" > "$dir/src/main.rs"

printf '%s\n' "$dir"
