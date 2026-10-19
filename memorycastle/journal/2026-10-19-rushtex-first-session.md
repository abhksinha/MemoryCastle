# First measured task on rushtex: two ledger holes and a span bug
Author: abhi+claude | Date: 2026-10-19
Files: src/code.rs, src/main.rs, plugin/hooks/after-read.py

## Context
A real task on rushtex (page-cache hit skips the font-usage scan, 3% on
the 500k stress document) run from a session whose project dir was this
repo, to see what the ledger reports on 110 KB files.

## Decision
- `'` opens a char literal in `strip_strings_and_comments` only when it
  closes within a few characters; a Rust lifetime or label (`Page<'_>`,
  `'outer:`) no longer blanks the rest of the line. Code index v9. Before,
  `page_key` printed as 173-176 and `push_page` as 269-270; both now end on
  the closing brace. Ten random rushtex spans checked, all end correctly.
- The read hook runs `memorycastle read` from the file's own directory, so
  a read in another repo than the session's lands in that repo's ledger.
  Before, every rushtex read from this session was dropped. Plugin 0.7.1.
- `links --context` records its `read:` caller lines as served spans, so a
  ranged read of a call site counts as guided.

## Measured
- Session ledger after the fixes (reads replayed through the fixed hook):
  3 guided reads, 3.3 KB read of 330 KB; observed 0-102k tokens (0-97%)
  net of 1k tokens of lookup. Floor is zero because a 40-line grep read
  costs the same as the span; the ceiling is what whole-file reads would
  have cost. Two guided reads were followed by a re-read of the same file:
  the truncated spans, since fixed.
- 49 tests pass; clippy clean.

## Open
- Floor zero on every task so far: the floor assumes an agent that greps
  and reads 40 lines, which is what a span read is. The honest claim is
  the ceiling only when the agent would otherwise read whole files; the
  `reads` table shows whether it does (rushtex: 104 KB read of 2.2 MB
  before any guidance, so it reads ranges already).
