# A note is held for once before compaction and before stop
Author: abhi+claude | Date: 2026-10-19
Files: src/main.rs, src/store.rs, plugin/hooks/pre-compact.sh, plugin/hooks/stop.sh, docs/GUIDE.md

## Context
The reminders at compaction and stop were advisory. The user asked for
journaling to be compulsory before compaction, where the why is lost for
good once summarised. Claude Code's `PreCompact` and `Stop` hooks block on
exit 2 with stderr as the reason to the agent.

## Decision
`memcastle owed --once <event>`: the first time per session and event that
a note is owed, exit 2 with the line on stderr; a `meta` key
`blocked:<event>:<project>` holds the session, so the second attempt exits
0. `pre-compact.sh` and `stop.sh` write the draft, call it, and on 2 pass
the reason with the draft path and the exact save command. Compulsory with
an escape: the agent writes the note while the facts are in context, or
says no decision was made and the next attempt passes. Nothing can wedge a
session that is out of context. Also: `status` lists a note as changed on
disk only when its bytes differ, not when a checkout moved its mtime.
Version 0.10.6.

## Rejected
- Blocking every time until a note is saved: an auto-compaction at the
  context limit with nothing worth a note would loop; the agent cannot
  always comply, and a hook must never hold a session hostage.
- Blocking on `git commit`: a commit closes a task but the agent is
  mid-turn with the facts in hand; context is enough there.

## Measured
Simulated an edit with no note after it: the first compaction hook exited 2
with the reason, the second 0; the stop hook the same. Six notes that a
checkout had touched stopped being reported as changed.
