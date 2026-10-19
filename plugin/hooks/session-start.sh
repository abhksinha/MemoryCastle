#!/bin/sh
# Once per session: declare the session to memorycastle (so savings can be
# shown per session), then print store state, stale instruction files and
# savings so far.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
sid=$(printf '%s' "$input" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$sid" ] && memcastle session "$sid" --agent claude >/dev/null 2>&1
# Warm model for the session: every lookup is 90 ms instead of 230 ms cold.
memcastle serve --detach >/dev/null 2>&1
memcastle status 2>/dev/null | grep -E '^(project:|notes:|observed|benchmark|label log|ceiling:|warning:|changed on disk)'
# After a compaction the draft written by pre-compact.sh is the one thing
# the summary must not lose; after a fresh start it is last session's debt.
owed=$(memcastle owed 2>/dev/null)
[ -n "$owed" ] && echo "memorycastle: $owed. Draft or append with: memcastle draft"
# The user-level CLAUDE.md carries the rules; the repo copy is for teammates.
root=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -n "$root" ] && [ -d "$root/memorycastle/journal" ] && ! grep -q 'memorycastle snippet' "$root/CLAUDE.md" 2>/dev/null; then
  echo "memorycastle: this repo's CLAUDE.md has no memory section; memcastle install --repo-only adds it for teammates"
fi
exit 0
