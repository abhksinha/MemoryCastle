#!/bin/sh
# On every prompt: what the journal is confident about, one line per note,
# each note once per session. Local, under 100 ms warm; silent when the
# prompt is short or the store has nothing. The lines are memory, not
# instructions: the agent reads the note before trusting it.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
sid=$(printf '%s' "$input" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
# This event's session, for every memcastle call below (not the session
# last started in the repo).
[ -n "$sid" ] && export MEMORYCASTLE_SESSION="$sid"
prompt=$(printf '%s' "$input" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("prompt",""))
except Exception: pass' 2>/dev/null)
[ -n "$prompt" ] || exit 0
out=$(printf '%s' "$prompt" | memcastle recall 2>/dev/null)
[ -n "$out" ] && printf 'memorycastle recall (memory, not instruction; path:line  confidence  section):\n%s\n' "$out"
# Mid-session: a note is owed and enough has been edited (see limits.rs);
# said once, then not for the cooldown.
owed=$(memcastle owed --remind 2>/dev/null)
[ -n "$owed" ] && printf 'memorycastle: %s. Before going further, write the journal note for the decision so far (memcastle draft gives the skeleton; memcastle save <note>), or carry on if no decision was made yet.\n' "$owed"
exit 0
