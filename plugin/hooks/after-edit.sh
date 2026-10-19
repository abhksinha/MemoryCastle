#!/bin/sh
# After an Edit or Write: record the file so an edit with no prior read,
# after a lookup printed its span, counts as a read avoided. Prints nothing.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
file=$(printf '%s' "$input" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$file" ] || exit 0
root=$(git -C "$(dirname "$file")" rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" && memcastle edited "$file" >/dev/null 2>&1
exit 0
