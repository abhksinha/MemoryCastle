#!/bin/sh
# Before a whole-file Read: if a search or links call already printed a line
# range for this file and nothing has read it, point the agent at the range
# and stop this read (exit 2). Once per span; a Read with offset/limit, or
# the retry, passes. Silent without memorycastle or outside a repo.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
sid=$(printf '%s' "$input" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
# This event's session, for every memcastle call below (not the session
# last started in the repo).
[ -n "$sid" ] && export MEMORYCASTLE_SESSION="$sid"
case "$input" in *'"offset"'*|*'"limit"'*) exit 0;; esac
file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$file" ] || exit 0
root=$(git -C "$(dirname "$file")" rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" && memcastle guard "$file"
