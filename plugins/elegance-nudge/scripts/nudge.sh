#!/usr/bin/env bash
# PostToolUse: run elegance on the file Claude just edited and report, as
# one line, only what is NEW since the last edit of that file this session.
#
# Tuned for the consumer, who is Claude, not a human tailing a log:
#   - never blocks. Advisory only, exit 0, stdout.
#   - never repeats. Findings are diffed against the previous run on this
#     file, so a problem is mentioned once. Ignoring it is allowed and
#     costs no further context; reintroducing it says so again.
#   - one line, grouped by unit, because "one function has four problems"
#     is a different fact from "four functions have one each".
#   - carries the budget (`cognitive 28>18`), since the value alone says
#     nothing about how far over the line it is.
#   - silent on a clean file, which is the common case and must cost zero.

set -euo pipefail

INPUT=$(cat)
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' 2>/dev/null)
SESSION=$(echo "$INPUT" | jq -r '.session_id // "nosession"' 2>/dev/null)

case "$TOOL" in
    Write|Edit|MultiEdit|NotebookEdit) ;;
    *) exit 0 ;;
esac
[ -n "$FILE" ] || exit 0
[ -f "$FILE" ] || exit 0
case "$FILE" in
    *.seed) exit 0 ;;  # recall fixtures are seeded smells by design
esac

# A plugin cannot ship a platform-specific binary, so it finds one. If
# there is none, say so ONCE per session and then be quiet: a hook that
# repeats an install hint on every edit is worse than one that does
# nothing.
STATE_DIR="${TMPDIR:-/tmp}/elegance-nudge/$SESSION"
ELEGANCE=""
for candidate in "$HOME/.local/bin/elegance" "$HOME/.cargo/bin/elegance" "$(command -v elegance 2>/dev/null || true)"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        ELEGANCE="$candidate"
        break
    fi
done
if [ -z "$ELEGANCE" ]; then
    mkdir -p "$STATE_DIR"
    if [ ! -f "$STATE_DIR/.missing" ]; then
        touch "$STATE_DIR/.missing"
        echo "elegance-nudge: no elegance binary on PATH or in ~/.local/bin — cargo install --git https://github.com/GrigoryEvko/elegance"
    fi
    exit 0
fi

JSON=$("$ELEGANCE" --json "$FILE" 2>/dev/null) || exit 0
SARIF=$("$ELEGANCE" --sarif "$FILE" 2>/dev/null) || exit 0

# One finding per line: rung, unit, startLine, metric, value, budget.
# SARIF carries the finding, --json carries the per-language budget; the
# join is what turns "cognitive 28" into "cognitive 28>18".
ROWS=$(jq -rn --argjson j "$JSON" --argjson s "$SARIF" '
  (($j.metrics // []) | map({key: .name, value: {hi: (.hi // 0), rung: (.rung // 9)}}) | from_entries) as $b
  | (($s.runs[0].results // [])[]
     | (.ruleId | sub("^elegance/"; "") | gsub("-"; " ")) as $m
     | ((.message.text // "") | ltrimstr($m) | ltrimstr(" ")) as $rest
     | ($rest | split(" in ")) as $p
     | [ ($b[$m].rung // 9),
         ($p[1] // ""),
         (.locations[0].physicalLocation.region.startLine // 0),
         $m,
         ($p[0] // ""),
         ($b[$m].hi // 0) ]
     | @tsv)
' 2>/dev/null) || exit 0
[ -n "$ROWS" ] || exit 0

# Report only what this edit introduced. State is per session and per
# file, so the first edit of a file reports where it stands and later
# edits report deltas.
mkdir -p "$STATE_DIR"
STATE="$STATE_DIR/$(printf '%s' "$FILE" | md5sum | cut -c1-16)"
CURRENT=$(echo "$ROWS" | awk -F'\t' '{print $4 "|" $2 "|" $5}' | sort -u)
if [ -f "$STATE" ]; then
    NEW_KEYS=$(comm -23 <(echo "$CURRENT") "$STATE" || true)
else
    NEW_KEYS="$CURRENT"
fi
echo "$CURRENT" > "$STATE"
[ -n "$NEW_KEYS" ] || exit 0

# Render: gate rungs (0-2, what CI would fail on) before suspicions,
# grouped by the unit they live in.
LINE=$(echo "$ROWS" | sort -t"$(printf '\t')" -k1,1n -k2,2 | awk -F'\t' -v keys="$NEW_KEYS" '
BEGIN { split(keys, k, "\n"); for (i in k) want[k[i]] = 1; cap = 5 }
{
    key = $4 "|" $2 "|" $5
    if (!(key in want)) next
    if (shown >= cap) { extra++; next }
    shown++
    at = ($2 == "" ? "file" : $2) ":" $3
    budget = ($6 == int($6) ? int($6) : $6)   # 11, not 11.0; 0.62 stays 0.62
    over = ($6 > 0 ? $5 ">" budget : $5)
    # A count metric whose budget is zero says everything with its name:
    # `secrets` and `swallowed` gain nothing from a trailing " 1".
    item = $4 (over == "1" ? "" : " " over)
    if (at == last) { printf ", %s", item }
    else { printf "%s%s %s", (last == "" ? "" : " · "), at, item; last = at }
}
END { if (extra) printf " (+%d more)", extra }
')
[ -n "$LINE" ] || exit 0

echo "elegance ${FILE##*/}: $LINE"
exit 0
