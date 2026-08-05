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

STATE_DIR="${TMPDIR:-/tmp}/elegance-nudge/$SESSION"
mkdir -p "$STATE_DIR"

# Local exemplars — the one intervention that measurably works.
#
# Shown three comments from the same file, this model's doc comments drop
# from 2.09x the length of the human original to 1.00x, and their semantic
# agreement with what the human actually wrote rises from 0.67 to 0.81
# against a 0.29 null. Nothing else measured here came close: every
# stylistic detector tried either collapsed under repo-grouped validation
# or turned out to be measuring register rather than quality.
#
# A PostToolUse hook fires after the write, so it cannot put exemplars in
# context beforehand. It delivers the same correction as feedback instead,
# and only when the file itself has established what normal looks like
# here — a file with too few comments has no local convention to appeal
# to, and guessing one would be worse than silence.
case "${FILE##*.}" in
    rs|ts|tsx|js|mjs|cjs|jsx|go|c|h|cc|cpp|hpp|cu|cuh|zig|java)
        CPAT='^[[:space:]]*//' ;;
    py|sh|bash) CPAT='^[[:space:]]*#[^!]' ;;
    lean)       CPAT='^[[:space:]]*--' ;;
    *)          CPAT='' ;;
esac

# Every contiguous comment run, as "<words>\t<text>". Markers and the
# leading decoration of doc comments are stripped so the count is prose.
runs() {
    awk -v pat="$1" '
        $0 ~ pat {
            l = $0; sub(pat, "", l); gsub(/^[[:space:]!\/*<>=-]+/, "", l)
            cur = (cur == "" ? l : cur " " l); inb = 1; next
        }
        { if (inb) { n = split(cur, w, " "); print n "\t" cur; cur = ""; inb = 0 } }
        END { if (inb) { n = split(cur, w, " "); print n "\t" cur } }
    '
}

# The comparison itself, as guard clauses: every reason to stay quiet is
# one line, and the interesting case falls out of the bottom.
verdict_on_length() {
    local pat=$1 file=$2 new=$3 wrote have count med sib
    [ -n "$pat" ] && [ -n "$new" ] || return 0
    wrote=$(printf '%s\n' "$new" | runs "$pat" | cut -f1 | sort -rn | head -1)
    [ -n "${wrote:-}" ] || return 0
    have=$(runs "$pat" < "$file" | cut -f1 | sort -n)
    count=$(printf '%s\n' "$have" | grep -c . || true)
    # Five comments is the floor for a median to mean anything.
    [ "${count:-0}" -ge 5 ] || return 0
    med=$(printf '%s\n' "$have" | awk '{a[NR]=$1} END{print (NR%2 ? a[(NR+1)/2] : int((a[NR/2]+a[NR/2+1])/2))}')
    # A median under four words is a file of `// TODO`-shaped notes, not a
    # documentation convention worth holding anything to.
    [ "${med:-0}" -ge 4 ] || return 0
    # 2x the local median is the effect size the paired experiment
    # measured; the 20-word floor keeps short files from firing on a
    # one-line comment that happens to be double a tiny median.
    [ "$wrote" -ge 20 ] && [ "$wrote" -gt $((med * 2)) ] || return 0
    sib=$(runs "$pat" < "$file" | awk -F'\t' -v m="$med" '
        $1 >= 4 && $1 <= m * 3 { d = $1 - m; if (d < 0) d = -d; print d "\t" $2 }' \
        | sort -n | head -2 | cut -f2 \
        | awk '{ s = $0; if (length(s) > 68) s = substr(s, 1, 65) "..."; printf "%s\"%s\"", (NR>1 ? " / " : ""), s }')
    [ -n "$sib" ] || return 0
    printf 'new comment %sw, file median %sw · %s' "$wrote" "$med" "$sib"
}

NEW_TEXT=$(echo "$INPUT" | jq -r '
    [ .tool_input.new_string // empty,
      .tool_input.content    // empty,
      ((.tool_input.edits // []) | map(.new_string) | join("\n")) ]
    | map(select(. != "")) | join("\n")' 2>/dev/null) || NEW_TEXT=""
SAID=$(verdict_on_length "$CPAT" "$FILE" "$NEW_TEXT")

# Said once per file per session, keyed on the length, so revising the
# same over-long comment does not repeat the same advice.
EXEMPLAR=""
EXKEY="$STATE_DIR/ex_$(printf '%s' "$FILE" | md5sum | cut -c1-16)"
if [ -n "$SAID" ] && [ "$(cat "$EXKEY" 2>/dev/null || true)" != "$SAID" ]; then
    printf '%s' "$SAID" > "$EXKEY"
    EXEMPLAR="elegance ${FILE##*/}: $SAID"
fi

# Anything below may exit early on a clean or unreadable file; the
# exemplar line is independent of elegance and must survive that.
finish() {
    if [ -n "$EXEMPLAR" ]; then echo "$EXEMPLAR"; fi
    exit 0
}

# A plugin cannot ship a platform-specific binary, so install.sh fetched
# the right one at SessionStart. A hand-installed elegance still wins:
# someone who built from source meant it. If there is none at all, say so
# ONCE per session and then be quiet — an install hint repeated on every
# edit is worse than a hook that does nothing.
ELEGANCE=""
for candidate in \
    "$(command -v elegance 2>/dev/null || true)" \
    "$HOME/.cargo/bin/elegance" \
    "$HOME/.local/bin/elegance" \
    "${CLAUDE_PLUGIN_DATA:-$HOME/.claude/elegance-nudge}/bin/elegance"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        ELEGANCE="$candidate"
        break
    fi
done
if [ -z "$ELEGANCE" ]; then
    if [ ! -f "$STATE_DIR/.missing" ]; then
        touch "$STATE_DIR/.missing"
        echo "elegance-nudge: no elegance binary — cargo install --git https://github.com/GrigoryEvko/elegance"
    fi
    finish
fi

JSON=$("$ELEGANCE" --json "$FILE" 2>/dev/null) || finish
SARIF=$("$ELEGANCE" --sarif "$FILE" 2>/dev/null) || finish

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
' 2>/dev/null) || finish
[ -n "$ROWS" ] || finish

# Report only what this edit introduced. State is per session and per
# file, so the first edit of a file reports where it stands and later
# edits report deltas.
STATE="$STATE_DIR/$(printf '%s' "$FILE" | md5sum | cut -c1-16)"
CURRENT=$(echo "$ROWS" | awk -F'\t' '{print $4 "|" $2 "|" $5}' | sort -u)
if [ -f "$STATE" ]; then
    NEW_KEYS=$(comm -23 <(echo "$CURRENT") "$STATE" || true)
else
    NEW_KEYS="$CURRENT"
fi
echo "$CURRENT" > "$STATE"
[ -n "$NEW_KEYS" ] || finish

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
[ -n "$LINE" ] || finish

if [ -n "$EXEMPLAR" ]; then echo "$EXEMPLAR"; fi
echo "elegance ${FILE##*/}: $LINE"
exit 0
