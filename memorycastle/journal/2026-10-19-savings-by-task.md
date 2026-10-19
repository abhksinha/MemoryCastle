# Savings by task against the repo's own unguided tasks
Author: abhi+claude | Date: 2026-10-19
Files: src/store.rs, src/importlog.rs, src/limits.rs, src/main.rs

## Context
The floor was zero on every task: per read, a span read and a 40-line grep
read cost the same. The unit was wrong. Grep is also not what the agent
does when the question has no symbol in it. The user asked for a method
whose floor is not zero by construction and that stays defensible.

## Decision
- `store::episodes`: a task is a ten-minute window from its first event
  (fixed, not chained: a chained gap merged an hour-long session into one
  task). A window with a lookup is guided; its cost is lookup output plus
  reads. The baseline is the median unguided window in the same repo,
  quartiles for spread, from the same ledger. Needs five unguided windows.
- `import-log --claude`: a Claude Code transcript becomes ledger rows at
  its own timestamps and session (`claude:<id>`): Read and Bash file reads
  with result sizes, a plain `memorycastle search|links` as a lookup capped
  at twice the search budget. Once per transcript (`meta` key).
- `status` prints `by task` for this session and all time, then
  `limits::BENCHMARK_LINE`, the controlled A/B of 2026-10-18, as the one
  number with no counterfactual. Plugin 0.8.0.

## Decision (same day, later)
No cumulative by-task line. The user's objection: a past unguided session
did other work, so its median is no counterfactual for today's guided
task; the sum over sessions compounds that. `by task` now compares guided
and unguided windows within one session only, and says "no comparison
yet" under five unguided windows. The clean figure stays the controlled
A/B, printed as the benchmark line.

## Rejected
- A Gaussian over read sizes: heavy-tailed, 26 points, a fit would report
  precision that does not exist. Quantiles need no model.
- Repo median read as a per-read floor: still per read; it cannot see the
  wrong-path reads a span skips, which only show per task.
- Counting any Bash line that mentions memorycastle as a lookup: benchmark
  loops in my own AbhiGram transcripts returned 93 KB each, 811k tokens of
  "lookup".

## Measured
- rushtex, old transcript imported: 623 reads, 38 unguided windows, median
  34.8 KB (quartiles 14.7-73.3). The test task today cost 14.3 KB: 6k
  tokens saved, 59%. All time: 7 guided windows at 78 KB each, -125%: the
  other six were development sessions reading whole files for tests.
- AbhiGram: 365 reads imported, 35 unguided windows, median 3.3 KB
  (1.5-16.0). All guided windows there are development sessions; -6762%.
  The line is honest about what it compares.
- memorycastle: no unguided window yet; the line says so.
- 50 tests pass; clippy clean.

## Open
- All-time figures carry the development sessions. A `status --since` or
  a per-session table would separate them; or purge sessions by name.
