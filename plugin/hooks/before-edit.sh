#!/bin/sh
# Before a file is edited: print what the journal says about it and where
# its symbols are, so the agent sees constraints without asking. Silent when
# memorycastle is absent, the file is outside a repo, or nothing links to it.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$file" ] || exit 0
root=$(git -C "$(dirname "$file")" rev-parse --show-toplevel 2>/dev/null) || exit 0
rel=${file#"$root"/}
case "$rel" in memorycastle/*) exit 0;; esac
# The line being edited, when the tool gives enough to find it: the notes
# about that definition, not every note about the file.
old_text=$(printf '%s' "$input" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("tool_input",{}).get("old_string","").split("\n")[0])
except Exception: pass' 2>/dev/null)
at=""
if [ -n "$old_text" ]; then
  at=$(grep -n -F -m1 -- "$old_text" "$file" 2>/dev/null | cut -d: -f1)
fi
if [ -n "$at" ]; then
  out=$(cd "$root" && memcastle links "$rel" --at "$at" --once 2>/dev/null)
else
  out=$(cd "$root" && memcastle links "$rel" --once 2>/dev/null)
fi
case "$out" in ""|"no notes link to"*) exit 0;; esac
printf 'memorycastle: notes about %s (read the printed ranges, not the file):\n%s\n' "$rel" "$out"
exit 0
