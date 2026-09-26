#!/usr/bin/env bash
# The math under incremental view maintenance (IVM). Every example is computed, every figure drawn by docs/draw.sh.
set -euo pipefail
(( BASH_VERSINFO[0] >= 4 )) || { echo "needs bash >= 4 (brew install bash)" >&2; exit 2; }

usage='usage:
  docs/ivm_math.sh            all chapters, color + glyphs
  docs/ivm_math.sh 3 6        chapters 3 and 6 only
  docs/ivm_math.sh --ascii    plain ASCII glyphs
  docs/ivm_math.sh --no-color no ANSI color (also honors NO_COLOR)
  docs/ivm_math.sh | less -R  page it'

color=1
glyphs=1
[[ -n "${NO_COLOR:-}" ]] && color=0
chapters=()
for arg in "$@"; do
  case "$arg" in
    --no-color) color=0 ;;
    --ascii) glyphs=0 ;;
    -h|--help) echo "$usage"; exit 0 ;;
    [0-9]*) chapters+=("$arg") ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

if (( color )); then
  H=$'\e[1;36m' K=$'\e[1m' G=$'\e[32m' R=$'\e[31m' D=$'\e[2m' M=$'\e[35m' B=$'\e[1m' N=$'\e[0m'
else
  H='' K='' G='' R='' D='' M='' B='' N=''
fi
source "$(dirname "${BASH_SOURCE[0]}")/draw.sh"

want() {
  (( ${#chapters[@]} == 0 )) && return 0
  local c
  for c in "${chapters[@]}"; do [[ "$c" == "$1" ]] && return 0; done
  return 1
}

render() {
  if (( glyphs )); then
    cat
  else
    sed -e 's/[─━═]/-/g' -e 's/[│┃║]/|/g' -e 's/[┌┐└┘├┤┬┴┼]/+/g' -e 's/█/#/g' \
        -e 's/Δ/d/g' -e 's/⋈/x/g' -e 's/⋉/x/g' -e 's/▷/>/g' -e 's/→/>/g' -e 's/⇒/>/g' \
        -e 's/▶/>/g' -e 's/◀/</g' -e 's/∅/0/g' -e 's/−/-/g' -e 's/●/o/g' -e 's/○/./g' \
        -e 's/∝/~/g' -e 's/·/*/g' -e 's/§/#/g' -e 's/′/'"'"'/g'
  fi
}

signed() { if (( $1 > 0 )); then printf '%s+%d%s' "$G" "$1" "$N"; else printf '%s−%d%s' "$R" "${1#-}" "$N"; fi; }

# "x:1 y:1" + "y:-1 z:1", printed as a Z-set with zero weights dropped.
zset_sum() {
  local -A w=(); local kv k out=""
  for kv in $1 $2; do k=${kv%%:*}; w[$k]=$(( ${w[$k]:-0} + ${kv#*:} )); done
  for k in $(printf '%s\n' "${!w[@]}" | sort); do (( w[$k] )) && out+="$k:${w[$k]}, "; done
  printf '{ %s }' "${out%, }"
}

zset_show() { local kv out=""; for kv in $1; do out+="${kv%%:*}:$(signed "${kv#*:}"), "; done; printf '{ %s }' "${out%, }"; }

# Rows are "left:right"; joins A (x:y) with B (y:z) on y and prints "x:z" rows.
join_rows() {
  local a b out=""
  for a in $1; do for b in $2; do [[ ${a#*:} == "${b%%:*}" ]] && out+="${a%%:*}:${b#*:} "; done; done
  printf '%s' "${out% }"
}

pairs() { local p out=""; for p in $1; do out+="(${p/:/, }) "; done; printf '%s' "${out% }"; }

# Semi-naive reach over "x:y" edges; fills ROUND_OF[x:z] and prints "round|delta|total" lines.
declare -A ROUND_OF=()
seminaive() {
  local edges=$1 delta=$1 next e d t round=0; local -A total=()
  ROUND_OF=()
  while [[ -n $delta ]]; do
    for d in $delta; do total[$d]=1; ROUND_OF[$d]=$round; done
    printf '%d|%s|%d\n' "$round" "$delta" "${#total[@]}"
    next=""
    for d in $delta; do
      for e in $edges; do
        t="${d%%:*}:${e#*:}"
        [[ ${d#*:} == "${e%%:*}" && -z ${total[$t]:-} && " $next " != *" $t "* ]] && next+="$t "
      done
    done
    delta=${next% }; round=$(( round + 1 ))
  done
  printf '%d||%d\n' "$round" "${#total[@]}"
}

ch1() {
  draw_chapter 1 "Z-sets: tables with signed counts"
  local a="x:1 y:1" d="y:-1 z:1"
  cat <<EOF
A ${B}Z-set${N} gives every row an integer ${B}weight${N}. Insert is ${G}+1${N}, delete is ${R}−1${N}.
A table is the rows whose accumulated weight is > 0.

Addition is row by row, computed:

    $(zset_show "$a")  +  $(zset_show "$d")  =  $(zset_sum "$a" "$d")

Every change is itself a Z-set ${K}ΔA${N}, and one step is ${K}A′ = A + ΔA${N}.
Inserts and deletes are the same operation with different signs.
EOF
}

ch2() {
  draw_chapter 2 "Linear operators: the change passes straight through"
  echo "    ${K}f(A + ΔA) = f(A) + f(ΔA)   ⇒   Δf(A) = f(ΔA)${N}    no memory, work ∝ |ΔA|"
  echo
  draw_flow "${G}ΔA${N}" "[f]" "${G}Δout${N}"
  cat <<EOF

The lab IR names its ops after ${B}MIR${N}, Materialize's mid-level relational IR.
${M}Mfp${N} is MIR's fused ${B}M${N}ap-${B}F${N}ilter-${B}P${N}roject: one linear pass per row.

EOF
  draw_table <<'EOF'
step | does | example
filter | drop rows | level > 30
map | append computed columns | level + 5
project | keep chosen columns | (trainer, move)
EOF
  echo
  local row lvl out=""
  for row in Lapras:40 Pidgey:5 Onix:31; do lvl=${row#*:}; (( lvl > 30 )) && out+="${G}+(${row%%:*}, $lvl)${N} "; done
  printf '    ΔA   = { +(Lapras, 40) +(Pidgey, 5) +(Onix, 31) }\n    Δout = { %s}   filter level > 30, computed\n\n' "$out"
  echo "Linear: ${M}Mfp  Union  Negate${N}."
}

ch3() {
  draw_chapter 3 "Join is bilinear: the growing rectangle"
  local A="Red:Lapras" dA="Red:Starmie Red:Gyarados" Bk="Lapras:Surf Starmie:Psychic" dB="Lapras:IceBeam Gyarados:HydroPump"
  draw_grid <<EOF
 | B (old) | ${G}Δb (new)${N}
A (old) | ${D}A ⋈ B${N}\n${D}already known${N} | ${G}A ⋈ Δb${N}
${G}Δa (new)${N} | ${G}Δa ⋈ B${N} | ${G}Δa ⋈ Δb${N}
EOF
  cat <<EOF

    ${K}(A+Δa) ⋈ (B+Δb) = A⋈B + Δa⋈B + A⋈Δb + Δa⋈Δb${N}

Same identity as the product rule d(ab) = da·b + a·db + da·db; the last term
stays because changes are whole rows. State: an index per input on the join key.

Computed on party(trainer, pokemon) ⋈ knows(pokemon, move):

EOF
  draw_table <<EOF
region | left | right | output
${D}A ⋈ B${N} | $(pairs "$A") | $(pairs "$Bk") | ${D}$(pairs "$(join_rows "$A" "$Bk")") skipped${N}
Δa ⋈ B | $(pairs "$dA") | $(pairs "$Bk") | ${G}$(pairs "$(join_rows "$dA" "$Bk")")${N}
A ⋈ Δb | $(pairs "$A") | $(pairs "$dB") | ${G}$(pairs "$(join_rows "$A" "$dB")")${N}
Δa ⋈ Δb | $(pairs "$dA") | $(pairs "$dB") | ${G}$(pairs "$(join_rows "$dA" "$dB")")${N}
EOF
}

ch4() {
  draw_chapter 4 "Threshold (distinct): emit only when the sign crosses zero"
  local steps="+1 +1 -1 -1" s total=0 prev i=1 totals="" body="" inset emit
  for s in $steps; do
    prev=$total; total=$(( total + s )); totals+="$total "
    inset="○"; emit="${D}(none)${N}"
    (( total > 0 )) && inset="●"
    (( prev <= 0 && total > 0 )) && emit="${G}+r${N}"
    (( prev > 0 && total <= 0 )) && emit="${R}−r${N}"
    body+="$i | $(signed "$s") | $total | $inset | $emit"$'\n'
    i=$(( i + 1 ))
  done
  echo "Set semantics ignores derivation counts. Threshold keeps each row's weight, computed:"
  echo
  { echo "step | Δw | total | in set? | emitted"; printf '%s' "$body"; } | draw_table
  echo
  draw_bars "$totals"
  echo
  echo "${B}Not linear.${N} State: one weight per row seen."
}

ch5() {
  draw_chapter 5 "Antijoin: negation built from join"
  echo "    ${K}A ▷ B  =  A  −  (A ⋉ distinct(B))${N}     rows of A with no match in B"
  echo
  draw_flow "A" "[⋈ distinct(B)]" "[negate]" "[union with A]" "A ▷ B"
  echo
  local trainers="Blue Green" wins="" t before after
  anti() { local out=""; for t in $trainers; do [[ " $wins " != *" $t "* ]] && out+="$t "; done; printf '%s' "${out% }"; }
  before=$(anti); wins="Blue"; after=$(anti)
  draw_table <<EOF
step | B = beaten trainers | A ▷ B = no loss | delta
0 | ∅ | $before | ∅
1 | ${G}+Blue${N} | $after | ${R}−(Blue)${N}
EOF
  echo
  echo "A change in B flips rows of A. Built from §2–§4 only: linear parts, one join, one threshold."
}

ch6() {
  draw_chapter 6 "Reduce: per-group old out, new in"
  cat <<EOF
    ${K}Δout(g) = − old(g) + new(g)${N}

${B}count, sum${N}: summable, so count(g) += Δw and sum(g) += Δw · value. O(1) per change.
${B}min, max${N}: deleting the minimum needs the next one. Bucket levels make a change one path:

EOF
  local before="5 9 3 7 8 2 6 4" after="5 9 3 7 8 10 6 4"; local -a leaves
  draw_min_tree "$before"
  echo
  echo "    change 2 → 10, recomputed; ${G}green${N} nodes changed:"
  echo
  draw_min_tree "$after" "$before"
  leaves=($before)
  echo
  echo "    work: $DRAW_CHANGED nodes on one path, not all ${#leaves[@]} values"
}

ch7() {
  draw_chapter 7 "Recursion: iterate until nothing changes"
  local edges="1:2 2:3 3:4" cut="2:3" kept="" r d t maxr=0
  cat <<EOF
    reach(x,y) :- edge(x,y).
    reach(x,z) :- reach(x,y), edge(y,z).

edges: $(pairs "$edges"). ${B}Semi-naive${N}: each round joins only the last round's Δ. Computed:

EOF
  {
    echo "round | Δreach | total"
    while IFS='|' read -r r d t; do
      if [[ -n $d ]]; then echo "$r | ${G}$(pairs "$d")${N} | $t"; else echo "$r | ∅  ${K}fixpoint${N} | $t"; fi
    done < <(seminaive "$edges")
  } | draw_table
  local -A before=() after=()
  seminaive "$edges" >/dev/null
  for t in "${!ROUND_OF[@]}"; do before[$t]=${ROUND_OF[$t]}; (( ROUND_OF[$t] > maxr )) && maxr=${ROUND_OF[$t]}; done
  for d in $edges; do [[ $d != "$cut" ]] && kept+="$d "; done
  seminaive "${kept% }" >/dev/null
  for t in "${!ROUND_OF[@]}"; do after[$t]=${ROUND_OF[$t]}; done
  cat <<EOF

${B}The delete problem${N}: cut edge (${cut/:/, }). In a cycle a→b→a each row supports the other,
so derivation counts never reach 0. ${M}DRed${N} (SQLite engine): over-delete, rederive, re-insert.
${M}Differential dataflow${N}: time is (epoch, round); a retraction cancels each row at the round
it was derived. Computed from the two fixpoints:

EOF
  local hdr="time" row0="epoch 0" row1="${R}epoch 1: cut${N}" c0 c1
  for (( r = 0; r <= maxr; r++ )); do
    c0=""; c1=""
    for t in $(printf '%s\n' "${!before[@]}" | sort); do
      (( before[$t] == r )) || continue
      c0+="${G}+(${t/:/,})${N}\\n"
      [[ -z ${after[$t]:-} ]] && c1+="${R}−(${t/:/,})${N}\\n"
    done
    hdr+=" | round $r"; row0+=" | ${c0%\\n}"; row1+=" | ${c1%\\n}"
  done
  printf '%s\n%s\n%s\n' "$hdr" "$row0" "$row1" | draw_grid
}

ch8() {
  draw_chapter 8 "Time, frontiers, compaction"
  echo "    input.advance_to(t)    \"no change before t will ever arrive\""
  echo
  draw_flow "${D}sealed history${N}" "[frontier t]" "${G}open changes${N}"
  cat <<EOF

Behind the frontier every difference is final, so the engine may add them together
(${B}compaction${N}). History of any length collapses to one Z-set per key: memory tracks
live data, not the number of steps.
EOF
}

ch9() {
  draw_chapter 9 "Summary: every IR op, its class, its state"
  draw_table <<EOF
op | class | state kept | incremental rule
${M}Mfp${N} | linear | none | Δout = f(ΔA)
${M}Union${N} | linear | none | Δout = ΔA + ΔB
${M}Negate${N} | linear | none | Δout = −ΔA
${M}Join${N} | bilinear | index per input | Δa⋈B + A⋈Δb + Δa⋈Δb
${M}Antijoin${N} | composite | right index + weights | §5
${M}Threshold${N} | nonlinear | weight per row | emit on zero crossing
${M}Reduce${N} | per group | accumulator per group | −old(g) + new(g)
${M}LetRec${N} | fixpoint | every recursive relation | semi-naive + DRed or 2-D time
EOF
}

{
  for n in 1 2 3 4 5 6 7 8 9; do want "$n" && "ch$n"; done
  true
} | render
