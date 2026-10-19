# Spans enforced by a read guard, and reads avoided counted
Author: abhi+claude | Date: 2026-10-19
Files: src/store.rs, src/main.rs, src/db.rs, plugin/hooks/before-read.sh, plugin/hooks/after-edit.sh, plugin/hooks/before-edit.sh

## Context
After a day of work the ledger showed 6 guided reads out of 36, 4 of them
followed by a whole-file read of the same file, and net savings of zero for
the session. Spans were printed and ignored, and the one case that saves the
most, a file never opened because search answered, counted nothing. The user
called it a serious flaw.

## Decision
- Read guard (`memorycastle guard`, PreToolUse on Read): when a lookup
  printed a line range for the file within `SPAN_FOLLOW_SECS` and no read of
  the file followed, the whole-file Read is stopped once (exit 2) with the
  ranges to read instead. A Read with offset or limit, and the retry, pass.
  `served_spans.guarded` (schema v18) makes it once per span.
- Reads avoided (`memorycastle edited`, PostToolUse on Edit, Write,
  MultiEdit, `edits` table): a file edited with no read in the session, after
  a served span for it, is credited at the whole file (ceiling) and a 40-line
  read (floor), once per file per session. Reported on the observed line as
  "files edited unread after a lookup".
- `links --once`: the edit hook looks a file up once per session (`meta` key
  `links:<session>:<path>`), so five edits of one file cost one lookup.
- Plugin 0.7.0.

## Rejected
- Blocking every whole-file read: a guard that fires twice for one span
  turns into a fight with the agent; once is a nudge, the ledger then shows
  whether it was followed.
- Crediting avoided reads without a served span: an edit with no read is
  then just an agent editing blind, nothing memorycastle did.

## Measured
- Simulation on this repo, fresh session: search printed
  `src/code.rs:412-434`; whole-file Read stopped once with that range, second
  attempt and a ranged Read passed; an Edit of `src/spans.rs` with no read
  credited 10.0 KB; `links --once` printed on the first edit of
  `src/store.rs`, nothing on the second. Observed line: 0-2k tokens
  (0-76%), net of 777 tokens of lookup output.
- 47 tests pass; clippy clean.

## Open
- The guard reads `file_path` from the hook JSON with sed; a path containing
  a quote is skipped. Fine for code, revisit if it fails on a real tree.
- Real figure needs a normal coding session with the plugin on.
