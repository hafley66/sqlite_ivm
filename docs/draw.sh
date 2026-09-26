#!/usr/bin/env bash
# Terminal drawing primitives, sourced by docs scripts. Widths ignore ANSI codes.
# Colors come from the caller: B D G R M N (empty strings disable them).
shopt -s extglob
export LC_ALL=${LC_ALL:-en_US.UTF-8}

draw_vlen() { local s=${1//$'\e['+([0-9;])m/}; printf '%d' "${#s}"; }

draw_rep() { local s; printf -v s '%*s' "$2" ''; printf '%s' "${s// /$1}"; }

draw_pad() {
  local t=$1 w=$2 a=${3:-l} n gap left
  n=$(draw_vlen "$t"); gap=$(( w - n )); (( gap < 0 )) && gap=0
  case $a in
    r) printf '%*s%s' "$gap" '' "$t" ;;
    c) left=$(( gap / 2 )); printf '%*s%s%*s' "$left" '' "$t" "$(( gap - left ))" '' ;;
    *) printf '%s%*s' "$t" "$gap" '' ;;
  esac
}

draw_trim() { local f=$1; f=${f#"${f%%[![:space:]]*}"}; printf '%s' "${f%"${f##*[![:space:]]}"}"; }

# Splits "a | b | c" into DRAW_F, keeping empty fields.
draw_split() {
  local raw f; DRAW_F=()
  IFS='|' read -r -a raw <<<"$1"
  for f in "${raw[@]}"; do DRAW_F+=("$(draw_trim "$f")"); done
}

draw_indent() { printf '%*s' "${DRAW_INDENT:-4}" ''; }

# stdin: "h1 | h2 | ..." then rows in the same form.
draw_table() {
  local -a rows=() w=(); local line i n r=0 ncol=0 t
  while IFS= read -r line; do [[ -n $line ]] && rows+=("$line"); done
  for line in "${rows[@]}"; do
    draw_split "$line"
    (( ${#DRAW_F[@]} > ncol )) && ncol=${#DRAW_F[@]}
    for i in "${!DRAW_F[@]}"; do n=$(draw_vlen "${DRAW_F[i]}"); (( n > ${w[i]:-0} )) && w[i]=$n; done
  done
  for line in "${rows[@]}"; do
    draw_split "$line"; draw_indent
    for (( i = 0; i < ncol; i++ )); do
      t=${DRAW_F[i]:-}; (( r == 0 )) && t="${B}$t${N}"
      draw_pad "$t" "${w[i]}"; (( i < ncol - 1 )) && printf '   '
    done
    echo
    if (( r == 0 )); then
      draw_indent
      for (( i = 0; i < ncol; i++ )); do printf '%s' "${D}$(draw_rep ─ "${w[i]}")${N}"; (( i < ncol - 1 )) && printf '   '; done
      echo
    fi
    r=$(( r + 1 ))
  done
}

# stdin: "corner | col1 | col2" then "label | cell | cell"; a literal \n splits a cell into lines.
draw_grid() {
  local -a rows=() w=(); local line i k n r lw=0 ncol h
  while IFS= read -r line; do [[ -n $line ]] && rows+=("$line"); done
  for line in "${rows[@]}"; do
    draw_split "$line"
    n=$(draw_vlen "${DRAW_F[0]}"); (( n > lw )) && lw=$n
    for (( i = 1; i < ${#DRAW_F[@]}; i++ )); do
      local -a parts=(); IFS=$'\n' read -r -d '' -a parts < <(printf '%b\0' "${DRAW_F[i]}")
      for k in "${parts[@]}"; do n=$(( $(draw_vlen "$k") + 2 )); (( n > ${w[i]:-0} )) && w[i]=$n; done
    done
  done
  draw_split "${rows[0]}"; ncol=${#DRAW_F[@]}
  local lead; lead="$(draw_indent)$(draw_rep ' ' "$lw") "
  printf '%s ' "$lead"
  for (( i = 1; i < ncol; i++ )); do draw_pad "${DRAW_F[i]}" "${w[i]}" c; printf ' '; done
  echo
  edge() { local l=$1 m=$2 e=$3; local s=$l; for (( i = 1; i < ncol; i++ )); do s+=$(draw_rep ─ "${w[i]}"); (( i < ncol - 1 )) && s+=$m; done; printf '%s%s%s\n' "$lead" "$s" "$e"; }
  edge ┌ ┬ ┐
  for (( r = 1; r < ${#rows[@]}; r++ )); do
    draw_split "${rows[r]}"
    local -a cells=("${DRAW_F[@]}") ; h=1
    for (( i = 1; i < ncol; i++ )); do
      local -a parts=(); IFS=$'\n' read -r -d '' -a parts < <(printf '%b\0' "${cells[i]:-}")
      (( ${#parts[@]} > h )) && h=${#parts[@]}
    done
    for (( k = 0; k < h; k++ )); do
      draw_indent; draw_pad "$( (( k == 0 )) && printf '%s' "${cells[0]}")" "$lw" r; printf ' │'
      for (( i = 1; i < ncol; i++ )); do
        local -a parts=(); IFS=$'\n' read -r -d '' -a parts < <(printf '%b\0' "${cells[i]:-}")
        draw_pad "${parts[k]:-}" "${w[i]}" c; printf '│'
      done
      echo
    done
    if (( r < ${#rows[@]} - 1 )); then edge ├ ┼ ┤; else edge └ ┴ ┘; fi
  done
}

# Tokens in [brackets] are operators.
draw_flow() {
  local out="" t
  for t in "$@"; do
    [[ -n $out ]] && out+=" ${D}──▶${N} "
    if [[ $t == \[*\] ]]; then out+="${M}$t${N}"; else out+="$t"; fi
  done
  draw_indent; printf '%s\n' "$out"
}

# Column chart of non-negative integers; optional second arg labels each column.
draw_bars() {
  local -a v=($1) lab=(${2:-}); local max=0 x y cw=6
  for x in "${v[@]}"; do (( x > max )) && max=$x; done
  for (( y = max; y >= 1; y-- )); do
    draw_indent; printf '%3d ┤' "$y"
    for x in "${v[@]}"; do if (( x >= y )); then printf '  %s██%s  ' "${G}" "${N}"; else printf '%*s' "$cw" ''; fi; done
    echo
  done
  draw_indent; printf '  0 ┼%s▶\n' "$(draw_rep ─ $(( ${#v[@]} * cw )))"
  draw_indent; printf '     '
  for x in "${!v[@]}"; do draw_pad "${lab[x]:-$((x + 1))}" "$cw" c; done
  echo
}

# Binary min tree over leaves; with a second leaf list, nodes that changed are green.
draw_min_tree() {
  local -a vals=($1) prev=(${2:-}) pos=() nv=() np=() npos=(); local slot=6 i j a b pa pb level=0 changed=0
  for i in "${!vals[@]}"; do pos[i]=$(( i * slot + slot / 2 )); done
  while :; do
    local line="" cur=0 t n start
    for i in "${!vals[@]}"; do
      t=${vals[i]}; n=${#t}; start=$(( pos[i] - n / 2 ))
      (( start > cur )) && line+=$(draw_rep ' ' $(( start - cur )))
      if (( ${#prev[@]} )) && [[ ${prev[i]} != "${vals[i]}" ]]; then line+="${G}$t${N}"; changed=$(( changed + 1 )); else line+=$t; fi
      cur=$(( start + n ))
    done
    draw_indent; printf 'level %d  %s' "$level" "$line"
    (( ${#vals[@]} == 1 )) && { printf '   %s◀── min%s\n' "${D}" "${N}"; break; }
    echo
    nv=(); np=(); npos=()
    for (( j = 0; j < ${#vals[@]}; j += 2 )); do
      a=${vals[j]}; b=${vals[j + 1]:-${vals[j]}}; nv+=($(( a < b ? a : b )))
      if (( ${#prev[@]} )); then pa=${prev[j]}; pb=${prev[j + 1]:-${prev[j]}}; np+=($(( pa < pb ? pa : pb ))); fi
      npos+=($(( (pos[j] + ${pos[j + 1]:-${pos[j]}}) / 2 )))
    done
    vals=("${nv[@]}"); prev=("${np[@]}"); pos=("${npos[@]}"); level=$(( level + 1 ))
  done
  DRAW_CHANGED=$changed
}

draw_rule() { printf '%s%s%s\n' "${D}" "$(draw_rep ═ 68)" "${N}"; }

draw_chapter() { echo; draw_rule; printf '%s  §%s  %s%s\n' "${H}" "$1" "$2" "${N}"; draw_rule; echo; }

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  if [[ -z ${NO_COLOR:-} ]]; then H=$'\e[1;36m' G=$'\e[32m' R=$'\e[31m' D=$'\e[2m' M=$'\e[35m' B=$'\e[1m' N=$'\e[0m'; else H='' G='' R='' D='' M='' B='' N=''; fi
  draw_chapter 0 "draw.sh primitives"
  echo "draw_table"; printf 'op | class\nJoin | bilinear\nMfp | linear\n' | draw_table; echo
  echo "draw_grid"; printf ' | col 1 | col 2\nrow 1 | a | b\\nb2\nrow 2 | c | d\n' | draw_grid; echo
  echo "draw_flow"; draw_flow "in" "[op]" "out"; echo
  echo "draw_bars"; draw_bars "1 3 2"; echo
  echo "draw_min_tree"; draw_min_tree "4 1 3 2" "4 5 3 2"
fi
