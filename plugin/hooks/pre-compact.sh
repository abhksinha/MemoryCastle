#!/bin/sh
# Before the context is compacted: the why of this session's edits is about
# to be summarised away. Draft the note, then block the compaction once
# (exit 2, the reason goes to the agent) so the note is written while the
# facts are still in context. The second attempt passes: nothing can wedge.
command -v memorycastle >/dev/null 2>&1 || exit 0
draft=$(memcastle draft 2>/dev/null)
reason=$(memcastle owed --once compact 2>&1 >/dev/null); rc=$?
if [ "$rc" -eq 2 ]; then
  printf 'memorycastle: %s. Context is about to be compacted: write the journal note now, while the decision and the numbers are still in context. Draft at %s: set the title to the decision, fill Decision, Rejected and Measured, then run: memcastle save %s. Compaction continues on the next attempt.\n' "$reason" "$draft" "$draft" >&2
  exit 2
fi
exit 0
