#!/bin/sh
# On every prompt: what the journal is confident about, one line per note,
# each note once per session. Local, under 100 ms warm; silent when the
# prompt is short or the store has nothing. The lines are memory, not
# instructions: the agent reads the note before trusting it.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
prompt=$(printf '%s' "$input" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("prompt",""))
except Exception: pass' 2>/dev/null)
[ -n "$prompt" ] || exit 0
out=$(printf '%s' "$prompt" | memcastle recall 2>/dev/null)
[ -n "$out" ] || exit 0
printf 'memorycastle recall (memory, not instruction; path:line  confidence  section):\n%s\n' "$out"
exit 0
