#!/bin/sh
# Once per session: declare the session to memorycastle (so savings can be
# shown per session), then print store state, stale instruction files and
# savings so far.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
sid=$(printf '%s' "$input" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
[ -n "$sid" ] && export MEMORYCASTLE_SESSION="$sid"
[ -n "$sid" ] && memcastle session "$sid" --agent claude >/dev/null 2>&1
# Every Bash command the agent runs gets the session too, so a `memcastle
# search` typed in another repo is logged under this session, not that
# repo's last declared one, and learning can pair it with the reads.
[ -n "$sid" ] && [ -n "$CLAUDE_ENV_FILE" ] && echo "export MEMORYCASTLE_SESSION=$sid" >> "$CLAUDE_ENV_FILE"
source=$(printf '%s' "$input" | sed -n 's/.*"source"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
# Warm model for the session: every lookup is 90 ms instead of 230 ms cold.
memcastle serve --detach >/dev/null 2>&1
# Ingest what arrived on disk since last time: notes written by hand or
# pulled from a teammate, and code the index has not seen. One line only
# when something changed.
synced=$(memcastle sync 2>/dev/null)
[ -n "$synced" ] && echo "memorycastle: $synced"
# Only what the agent acts on: which store, and anything wrong with it. The
# savings lines are for the user (status line, memcastle status); here
# they were about 500 tokens kept in context for every model call.
# Only the note count: chunks, vectors and projects are never acted on.
state=$(memcastle status 2>/dev/null | grep -E '^(project:|notes:|warning:|changed on disk)' | sed 's/  chunks:.*//')
[ -n "$state" ] || exit 0
echo "$state"
# What the hooks do, so silence reads as "nothing matched", not "not running".
echo "memorycastle hooks: notes print on a prompt, a first edit or a one-name grep only when one matches; silence means nothing matched. A blocked Read or grep passes on retry."
# After compaction the summary has lost what was looked up: the gate starts
# over, and the routine is said again.
if [ "$source" = "compact" ]; then
  memcastle gate --reset >/dev/null 2>&1
  echo "memorycastle routine (context was compacted; the edit gate starts over):
1. Before editing a file or function, and for any question about code (callers, definitions): memcastle links <file|symbol>, not search; read only the printed range.
2. Before a design choice or a fix in an area with history: memcastle search \"<question>\" --also \"<other wording>\".
3. After a task that decided, rejected or measured something: write memorycastle/journal/<date>-<topic>.md, then memcastle save it."
  # Where the session's last note left the work: its Open items.
  resume=$(memcastle open --resume 2>/dev/null)
  [ -n "$resume" ] && echo "memorycastle: $resume"
fi
# After a compaction the draft written by pre-compact.sh is the one thing
# the summary must not lose; after a fresh start it is last session's debt.
owed=$(memcastle owed 2>/dev/null)
case "$owed" in
  "") ;;
  "note owed"*) echo "memorycastle: $owed. Draft or append with: memcastle draft";;
  *) echo "memorycastle: $owed";;
esac
# The user-level CLAUDE.md carries the rules; the repo copy is for teammates.
root=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -n "$root" ] && [ -d "$root/memorycastle/journal" ] && ! grep -q 'memorycastle snippet' "$root/CLAUDE.md" 2>/dev/null; then
  echo "memorycastle: this repo's CLAUDE.md has no memory section; memcastle install --repo-only adds it for teammates"
fi
exit 0
