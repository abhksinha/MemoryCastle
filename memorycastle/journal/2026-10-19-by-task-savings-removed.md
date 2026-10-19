# By-task savings removed; the observed figure and the A/B are what hold
Author: abhi+claude | Date: 2026-10-19
Files: src/store.rs, src/main.rs, src/limits.rs, src/install.rs, plugin/hooks/session-start.sh
Supersedes: 2026-10-19-savings-by-task.md

## Context
The by-task line compared ten-minute windows that held a lookup against
the session's own windows that did not. On the first real session it read
"50k tokens more than the baseline (2.4x)". The `tasks` table showed why,
and the why was the method.

## Decision
Removed: `store::episodes`, the `tasks` command, the `by task` line and
the status-line segment, `EPISODE_MIN_BASELINE`. `status` and the status
line report the observed figure (bytes of files a printed span stood in
for, minus what was read and the lookup output; written with a tilde as
an upper bound) and the controlled A/B line. Nothing else claims a number.

## Rejected
- Per-file-touched normalisation: built, still 2.3x, because the guided
  windows held bulk reads the lookup had nothing to do with.
- Subtracting "development" reads: any such choice is the cherry-picking
  the design refused on day one.
- Keeping it as an opt-in diagnostic: a number that can swing from 50%
  saved to 2.4x worse on the same tool teaches nothing and costs 200 lines.

## Measured
This session's windows: a guided window of 3 lookups and 13 reads at
94 KB (editing `src/eval.rs`), another of 34 lookups and one read (a
benchmark loop), and an unguided window of one 443-byte read counted as
a task. Median unguided task 10.3 KB against 24.7 KB guided; per file
6.5 KB against 2.8 KB. The attribution, not the tool, produced both.

## Open
The A/B needs rerunning when a claim is wanted; the observed figure stays
inspectable in `reads` and `served_spans`.
