# The hooks ask for a note at the three moments it is due
Author: abhi+claude | Date: 2026-10-19
Files: src/draft.rs, src/main.rs, plugin/hooks/pre-compact.sh, plugin/hooks/after-bash.sh, plugin/hooks/session-start.sh, plugin/hooks/stop.sh, plugin/hooks/hooks.json

## Context
The whole design bets on the agent writing the note instead of a model
extracting one later. The weakest link is the agent forgetting. The stop
hook caught the end of a session; it missed the two moments that lose the
most: compaction, when the why is about to be summarised away, and a
commit, which closes a task mid-session.

## Decision
One question, `draft::owed`: files edited in this session, outside
`memorycastle/`, at or after the last note this session saved (or the
session start). `memcastle owed` prints it in one line, silent otherwise.
Three hooks ask it: `PreCompact` writes the draft and says so, and the
post-compaction session start repeats where it is, because that line is
injected into the new context; a `git commit` in Bash returns it to the
agent as `additionalContext` with "write it if this closed a task,
otherwise ignore", so the user sees nothing; stop keeps its reminder.
`draft::write` now triggers on edits after the last note, not only on
sessions with no note at all. Version 0.10.4.

## Rejected
- Reminding on every edit or every N minutes: noise, and the agent learns
  to ignore it. Three moments with a real boundary is the whole list.
- Writing the note for the agent from the diff: that is extraction, the
  thing the design refuses; the draft fills only what the ledger knows
  (files, author, date) and leaves Decision to the one who decided.

## Measured
In this session: `owed` reported one file edited since the last note
(`docs/index.html`), the commit hook returned it as context, and the
pre-compaction hook wrote the draft in under 50 ms.
