# Quality scoring stays linear above 5,000 chunks
Author: abhi+claude | Date: 2026-10-19
Files: src/quality.rs, src/limits.rs

## Context
`quality::notes` compared every chunk pair: fine at 3,619 chunks, a
minute at 50,000. The save path called the same full pass for one note.

## Decision
Above `QUALITY_MAX_CHUNKS` (5,000) only the `QUALITY_RECENT_NOTES` (200)
newest notes are subjects; every pair with no subject on either side is
skipped, so the pass is subjects times store. `of_note` scores the saved
note alone the same way, linear in the store at any size. Under the bound
the full table and its percentiles stay.

## Rejected
- Sampling neighbours: a duplicate is one specific note; a sample misses it.
- Approximate neighbours through the bit pack: the pack is per project and
  rebuilt on writes; wiring it in for a command run once a day is not
  worth the code until a store is over the bound.

## Measured
`quality` on 29 notes, 139 chunks: 0.42 s wall, dominated by the SQLite
reads of vectors and the label-log join, under 10 ms of dot products.
The over-bound path is untested against a real store of that size.

## Open
Run the 700-note scale fixture through `quality` once to time the
recent-notes path and check percentiles still fire on the subjects only.
