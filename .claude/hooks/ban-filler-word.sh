#!/usr/bin/env bash
# PreToolUse hook: ban the word "pre-existing" (and the variants
# "preexisting" / "pre existing", any case) in content this agent is
# about to write.
#
# Rationale: "pre-existing" is filler that hides the actual prior state.
# Name the real thing instead ("the existing X", "the longstanding Y",
# "the drift introduced in commit Z").
#
# Wired from .claude/settings.json as a PreToolUse hook on Write|Edit|Bash.
# It inspects only the text THIS agent authors — Write `.content`,
# Edit `.new_string` (+ MultiEdit `.edits[].new_string`), and Bash
# `.command` (catches git commit messages). It deliberately ignores
# Edit `.old_string` so removing the banned word is never blocked.
#
# Decision protocol: emit a PreToolUse JSON verdict with
# permissionDecision="deny" when the word is found; otherwise stay
# silent (exit 0 = allow).
set -euo pipefail

payload="$(cat)"

text="$(printf '%s' "$payload" | jq -r '
  [
    .tool_input.content,
    .tool_input.new_string,
    .tool_input.command,
    (.tool_input.edits // [] | .[]?.new_string)
  ] | map(select(. != null)) | join("\n")
' 2>/dev/null || true)"

if printf '%s' "$text" | grep -iqE 'pre[-_ ]?existing'; then
  cat <<'JSON'
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"The word 'pre-existing' (and variants: preexisting, pre existing) is banned in this project. Name the actual prior state instead — e.g. 'the existing X', 'longstanding Y', or 'drift from commit Z'."}}
JSON
fi

exit 0
