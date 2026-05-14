#!/bin/bash
# Vet HANDOFF.md against git log on session start.
# Warns about items that appear completed based on recent commits.
# Output goes to the session transcript so /prime sees it.

set -euo pipefail

# Get working directory from hook stdin JSON, fall back to pwd
CWD=$(cat /dev/stdin 2>/dev/null | grep -o '"cwd":"[^"]*"' | head -1 | cut -d'"' -f4 || true)
CWD="${CWD:-$(pwd)}"
cd "$CWD" 2>/dev/null || exit 0

HANDOFF="$CWD/HANDOFF.md"

# No HANDOFF.md? Nothing to vet.
if [ ! -f "$HANDOFF" ]; then
  exit 0
fi

# Not a git repo? Can't cross-reference.
if ! git rev-parse --is-inside-work-tree &>/dev/null; then
  exit 0
fi

# Extract the date from HANDOFF.md (looks for **Date:** YYYY-MM-DD)
HANDOFF_DATE=$(grep -oP '(?<=\*\*Date:\*\*\s)[\d-]+' "$HANDOFF" 2>/dev/null || true)

# Extract unchecked items (- [ ] lines)
OPEN_ITEMS=$(grep '^\s*- \[ \]' "$HANDOFF" 2>/dev/null || true)

if [ -z "$OPEN_ITEMS" ]; then
  exit 0
fi

# Get git log since the handoff date (or last 30 commits if no date)
if [ -n "$HANDOFF_DATE" ]; then
  GIT_LOG=$(git log --oneline --since="$HANDOFF_DATE" 2>/dev/null || true)
else
  GIT_LOG=$(git log --oneline -30 2>/dev/null || true)
fi

if [ -z "$GIT_LOG" ]; then
  exit 0
fi

# Convert git log to lowercase for matching
GIT_LOG_LOWER=$(echo "$GIT_LOG" | tr '[:upper:]' '[:lower:]')

# Check each open item against the git log
STALE_ITEMS=""
STALE_COUNT=0

while IFS= read -r item; do
  # Strip the checkbox prefix and whitespace
  clean_item=$(echo "$item" | sed 's/^\s*- \[ \]\s*//')

  # Skip empty lines
  [ -z "$clean_item" ] && continue

  # Extract keywords (3+ char words) from the item
  keywords=$(echo "$clean_item" | tr '[:upper:]' '[:lower:]' | grep -oE '[a-z]{3,}' | sort -u)

  # Count how many keywords appear in the git log
  match_count=0
  total_keywords=0
  for kw in $keywords; do
    total_keywords=$((total_keywords + 1))
    if echo "$GIT_LOG_LOWER" | grep -q "$kw"; then
      match_count=$((match_count + 1))
    fi
  done

  # If more than half the keywords match, flag as potentially stale
  if [ "$total_keywords" -gt 0 ] && [ "$match_count" -gt 0 ]; then
    threshold=$(( (total_keywords + 1) / 2 ))
    if [ "$match_count" -ge "$threshold" ]; then
      STALE_ITEMS="${STALE_ITEMS}\n  - ${clean_item}"
      STALE_COUNT=$((STALE_COUNT + 1))
    fi
  fi
done <<< "$OPEN_ITEMS"

# Output warnings if stale items found
if [ "$STALE_COUNT" -gt 0 ]; then
  TOTAL_OPEN=$(echo "$OPEN_ITEMS" | grep -c '^\s*- \[ \]' || true)
  echo "=== HANDOFF.md Vet ==="
  echo "HANDOFF.md has ${TOTAL_OPEN} open items. ${STALE_COUNT} may already be completed based on git history:"
  echo -e "$STALE_ITEMS"
  echo ""
  if [ -n "$HANDOFF_DATE" ]; then
    echo "Handoff written: ${HANDOFF_DATE} | Commits since: $(echo "$GIT_LOG" | wc -l | tr -d ' ')"
  fi
  echo "When running /prime or /handoff, cross-reference these against git log and remove completed items."
  echo "======================"
fi

exit 0
