# Notes stay prose; titles state the decision; authorship filled by the session
Author: abhi+claude | Date: 2026-10-19
Files: src/app.rs, src/store.rs, src/chunk.rs, plugin/hooks/session-start.sh

## Context
The user asked whether notes should be written for the embedding model,
with a tabulated template, and whether to record line numbers; and whether
the Author header can answer "who made this change" across agents.
Reviewed all 25 notes here and 10 in AbhiGram.

## Decision
Prose stays. The title is embedded with every chunk (`title: {title} |
text:` prefix), so a title must state the decision, not the task. A section
under 200 words is one chunk, so Decision stays under 120 words with the
choice in its first sentence. Measured is sentences with one number each;
a table only for a real matrix under eight rows. No line numbers in notes:
they rot on the next commit; a backticked symbol resolves to a live span.
Author becomes reliable by filling it at `save` from the session's agent and
the git user when the header lacks it; "who" questions are a filter on
`links` and `search` by author and date, not a search.

## Rejected
- Tabulated template: cells embed as fragments with no subject, the splitter
  repeats header rows, and the 45 KB table note in AbhiGram lost 30% in the
  agent benchmark.
- Line numbers as references: stale within a week and look authoritative.
- git blame for authorship: line ownership lives in git; the note records
  who decided and why.

## Measured
- memorycastle journal: 23 of 25 notes lint-clean; four batch notes bundle
  4 to 7 decisions, Decision sections average 90 words, one at 400.
- AbhiGram journal: 8 of 10 notes lack `Files:`, trackers at 2,750 and
  5,943 words, lint fires 6 to 11 times per file; every near-miss today
  came from those files.

## Open
- Lint: Decision over 120 words or more than three bullets; task-like
  titles ("batch", "review", "session").
- `save` fills `Author:` from `memorycastle session <id> --agent <name>`
  and git config; `--author` filter and `who <file|symbol>` later.
