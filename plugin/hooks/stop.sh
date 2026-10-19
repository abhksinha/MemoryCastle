#!/bin/sh
# When the agent stops: unsaved journal notes are reminded; a session that
# edited code after its last note is held once (exit 2, the reason goes to
# the agent) so the note gets written. The second stop passes.
command -v memorycastle >/dev/null 2>&1 || exit 0
changed=$(memcastle status 2>/dev/null | sed -n 's/^changed on disk since saved: //p')
[ -n "$changed" ] && printf 'memorycastle: unsaved journal notes: %s. Run: memcastle save --changed\n' "$changed"
draft=$(memcastle draft 2>/dev/null)
reason=$(memcastle owed --once stop 2>&1 >/dev/null); rc=$?
if [ "$rc" -eq 2 ]; then
  printf 'memorycastle: %s. Before stopping, write the journal note if this task changed a decision, measured something or rejected an approach. Draft at %s: set the title to the decision, fill the sections, then: memcastle save %s. If no decision was made, stop again and say so.\n' "$reason" "$draft" "$draft" >&2
  exit 2
fi
exit 0
