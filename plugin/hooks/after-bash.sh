#!/bin/sh
# After a Bash command: a `git commit` ends a task, so ask once whether a
# note is owed for the code edited since the last one. The answer goes to
# the agent as context, not to the user.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
printf '%s' "$input" | grep -q '"command"[[:space:]]*:[[:space:]]*"[^"]*git commit' || exit 0
owed=$(memcastle owed 2>/dev/null)
[ -n "$owed" ] || exit 0
esc=$(printf '%s' "$owed" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"memorycastle: %s. If this commit closed a task that changed a decision, measured something or rejected an approach, write the journal note now (memcastle draft writes the skeleton) and save it; otherwise ignore."}}\n' "$esc"
exit 0
