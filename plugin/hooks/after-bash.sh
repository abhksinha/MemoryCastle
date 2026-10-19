#!/bin/sh
# After a Bash command. Two jobs:
# - a `git commit` ends a task, so ask once whether a note is owed for the
#   code edited since the last one;
# - any other command may have edited files through the shell (heredoc,
#   sed -i, a script), which the Edit hook never sees: look up each changed
#   file once, as the Edit hook would, and record the edit.
# The answer goes to the agent as context, not to the user.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
esc() { printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null; }
if printf '%s' "$input" | grep -q '"command"[[:space:]]*:[[:space:]]*"[^"]*git commit'; then
  owed=$(memcastle owed 2>/dev/null)
  [ -n "$owed" ] || exit 0
  printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"memorycastle: %s. If this commit closed a task that changed a decision, measured something or rejected an approach, write the journal note now (memcastle draft writes the skeleton) and save it; otherwise ignore."}}\n' "$(esc "$owed")"
  exit 0
fi
changed=$(memcastle changed 2>/dev/null)
[ -n "$changed" ] || exit 0
printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"memorycastle: %s"}}\n' "$(esc "$changed")"
exit 0
