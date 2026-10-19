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
exit 0
