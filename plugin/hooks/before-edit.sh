#!/bin/sh
# Before Edit, Write or MultiEdit: hand the event to `memcastle gate --hook`,
# which reads the file, the line and the new text from the JSON and answers
# in Claude Code's shape. Everything the gate does lives in the binary, so a
# new binary changes it without reinstalling this plugin: the first edit of
# a file gets its lookup, the first edit in an area a search, and an edit
# that brings back an approach a linked note rejected gets that note's line.
# Context by default; `memcastle project gate block` (or MEMORYCASTLE_GATE=block)
# stops the edit once (exit 2).
# Silent when memorycastle is absent or the file is outside a repo.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$file" ] || exit 0
dir=$(dirname "$file")
while [ ! -d "$dir" ]; do dir=$(dirname "$dir"); done
root=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
printf '%s' "$input" | exec memcastle gate --hook
