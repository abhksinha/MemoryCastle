#!/bin/sh
# When the agent stops: unsaved journal notes are reminded; a session that
# edited code after its last note is held once (exit 2, the reason goes to
# the agent) so the note gets written, and once more when the saved note's
# Files: line leaves out files edited for it. The next stop passes.
command -v memorycastle >/dev/null 2>&1 || exit 0
input=$(cat)
sid=$(printf '%s' "$input" | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
# This event's session, for every memcastle call below (not the session
# last started in the repo).
[ -n "$sid" ] && export MEMORYCASTLE_SESSION="$sid"
st=$(memcastle status 2>/dev/null)
changed=$(printf '%s\n' "$st" | sed -n 's/^changed on disk since saved: //p')
msg=""
[ -n "$changed" ] && msg="memorycastle: unsaved journal notes: $changed. Run: memcastle save --changed. "
draft=$(memcastle draft 2>/dev/null)
reason=$(memcastle owed --once stop 2>&1 >/dev/null); rc=$?
if [ "$rc" -eq 2 ] && [ -z "$draft" ]; then
  # Nothing owed, but the saved note's Files: line misses edited files.
  printf 'memorycastle: %s. If those edits belong to no decision, stop again and say so.\n' "$reason" >&2
  exit 2
fi
if [ "$rc" -eq 2 ]; then
  printf 'memorycastle: %s. Before stopping, write the journal note if this task changed a decision, measured something or rejected an approach. Draft at %s: set the title to the decision, fill the sections, then: memcastle save %s. If no decision was made, stop again and say so.\n' "$reason" "$draft" "$draft" >&2
  exit 2
fi
# Shown to the user, not the agent: code files edited with no links of the
# agent's own this session.
usage=$(printf '%s\n' "$st" | sed -n 's/^this session: \(.*with no links of your own.*\)$/\1/p')
[ -n "$usage" ] && msg="${msg}memorycastle this session: $usage"
# One JSON object on stdout: a plain line on exit 0 reaches nobody.
[ -n "$msg" ] && printf '{"systemMessage": "%s"}\n' "$(printf '%s' "$msg" | sed 's/\\/\\\\/g; s/"/\\"/g')"
exit 0
