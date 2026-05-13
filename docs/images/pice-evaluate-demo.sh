#!/usr/bin/env bash
# Mock renderer for the README Stack Loops GIF.
# Keep this deterministic: it must not require provider API keys or a daemon.

set -euo pipefail
trap 'printf "\033[?25h"' EXIT
printf '\033[?25l'

inner_width=74
bar_width=48
title="m0lz.02 Stack Loops"

lines=(
  '$ npm install -g @jacobmolz/pice'
  'pice and pice-daemon installed'
  '$ pice init'
  'created .pice/workflow.yaml and .claude/'
  '$ pice layers detect --json'
  'detected infrastructure, database, api, frontend'
  '$ pice evaluate .claude/plans/stack-loops.md --background --wait'
  'daemon admitted run r-019e220e and streamed manifest updates'
  'infrastructure  passed   seam checks clean'
  'database        passed   migrations verified'
  'api             passed   provider contract isolated'
  'frontend        passed   review gate approved and resumed'
  '$ pice status stack-loops-phase8 --json'
  'overall_status=passed confidence=0.94'
)

kinds=(
  cmd
  ok
  cmd
  ok
  cmd
  ok
  cmd
  out
  ok
  ok
  ok
  ok
  cmd
  out
)

color_for() {
  case "$1" in
    cmd) printf '\033[38;5;255m' ;;
    ok) printf '\033[38;5;115m' ;;
    *) printf '\033[38;5;250m' ;;
  esac
}

repeat_char() {
  local count="$1"
  local char="$2"
  printf '%*s' "$count" '' | tr ' ' "$char"
}

print_text_line() {
  local color="$1"
  local text="$2"
  printf '\033[38;5;240m|\033[0m %b%-*s\033[0m \033[38;5;240m|\033[0m\n' \
    "$color" "$inner_width" "$text"
}

render_frame() {
  local visible="$1"
  local total="${#lines[@]}"
  local progress=$((visible * 100 / total))
  local filled=$((visible * bar_width / total))
  local empty=$((bar_width - filled))
  local bar

  bar="$(repeat_char "$filled" '#')$(repeat_char "$empty" '.')"

  printf '\033[2J\033[H'
  printf '\033[38;5;240m+%s+\033[0m\n' "$(repeat_char $((inner_width + 2)) '-')"
  printf '\033[38;5;240m|\033[0m \033[1;37m%-*s\033[0m \033[38;5;245m%3d%%\033[0m \033[38;5;240m|\033[0m\n' \
    $((inner_width - 5)) "$title" "$progress"
  printf '\033[38;5;240m|\033[0m %-*s \033[38;5;240m|\033[0m\n' "$inner_width" ''

  for index in "${!lines[@]}"; do
    if ((index < visible)); then
      print_text_line "$(color_for "${kinds[$index]}")" "${lines[$index]}"
    else
      print_text_line '' ''
    fi
  done

  printf '\033[38;5;240m|\033[0m %-*s \033[38;5;240m|\033[0m\n' "$inner_width" ''
  printf '\033[38;5;240m|\033[0m \033[38;5;255m[%s]\033[0m %-*s \033[38;5;240m|\033[0m\n' \
    "$bar" $((inner_width - bar_width - 2)) ''
  printf '\033[38;5;240m+%s+\033[0m\n' "$(repeat_char $((inner_width + 2)) '-')"
}

sleep 0.25

for visible in $(seq 1 "${#lines[@]}"); do
  render_frame "$visible"
  sleep 0.35
done

for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
  render_frame "${#lines[@]}"
  sleep 0.5
done
