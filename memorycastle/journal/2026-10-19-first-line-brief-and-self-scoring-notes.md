# First line is confidence and location; the store scores its own notes
Author: abhi+claude | Date: 2026-10-19
Files: src/render.rs, src/spans.rs, src/quality.rs, src/draft.rs, src/store.rs, src/eval.rs, plugin/hooks/before-bash.py, plugin/hooks/stop.sh

## Context
Users on large projects pay for every token a lookup prints and depend on
notes other people wrote. The user asked for a minimal first line, and for
the store to tell an agent when its notes are weak, with thresholds that
move as the journal grows.

## Decision
Search prints `n. <confidence>  <path>:<line> > <section>` first, the
title and author second; `--brief` stops at the first line. Weak span
guesses are dropped when a strong span exists (`SPAN_STRONG`). The store
scores notes from stored vectors and the label log (`quality.rs`): spread,
overlap, served-but-never-opened, flagged by percentiles from 30 notes up.
`save` fills `Author:` and `Date:`, names edited files `Files:` omits, and
prints the flags; the stop hook drafts a note from the edits ledger when a
session saved none; a Bash `cat` or long `sed -n` is guarded like Read.

## Rejected
- Per-store cosine calibration from the gold set: built, measured, removed.
  Right-note median cosine 0.75, best-wrong 90th percentile 0.76 to 0.79 on
  both sets; floor would sit above ceiling. The cosine says "on topic",
  fusion and the gate pick among related notes. `eval` keeps the quantiles.
- JSON from the model: an embedding model outputs a vector, nothing else.
- Line numbers in notes: stale within a week; spans re-anchor.
- Date and author on the first line: a dated note's name carries the date,
  the author is on the title line; 15 tokens per hit instead of 40.

## Measured
- First line 47 bytes against 130 for the two lines it replaces.
- `quality` on 27 notes: spreads 0.79 to 0.82, overlaps up to 0.87, no
  verdicts under the 30-note floor; one pass under 10 ms.
- Draft, Bash guard and author fill verified in a scratch repo: guard
  exits 2 once with `lines 2-4`, passes the retry and a 20-line range.
- Gold after the changes: AbhiGram recall@3 1.000, MRR 0.905; own set
  0.962, 0.892 (two ambiguous questions now third behind newer notes).

## Open
- Thresholds are percentiles; the gold sets stay the anchor against a
  journal that degrades as a whole.
- `quality` is one pass over all chunk pairs; above about 5,000 chunks it
  needs the recent-notes-only path.

## Decision (same day, later)
Three things the first real session exposed, fixed:
- A caller is a call: `spans::is_call` requires `name(`, `name!(`,
  `x.name(` or `name::` with identifier boundaries, strings and comments
  stripped. `links quality::notes --context` listed four files that only
  contained the word `notes`; now one true caller and one false one, a
  `REFERENCES notes(id)` line inside a multi-line SQL string, which a
  line scanner cannot see is a string.
- `--sections` prints `open:` when a hit landed in Open, where a task's
  instruction lives.
- `by task` says how far the session is from a comparison (`11 guided, 3
  of 5 unguided`) instead of "no comparison yet"; the status line shows
  the observed ceiling as an absolute (`up to 49k tokens (63%) not read`)
  beside it. A label-log open counts only `OPEN_MIN_SECS` after the serve,
  so a benchmark loop is not a reader; `MEMORYCASTLE_NO_LEDGER` keeps
  loops out of every ledger.
