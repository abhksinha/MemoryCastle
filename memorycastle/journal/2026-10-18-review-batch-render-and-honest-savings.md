# Expert review batch: near-miss gate, shared render, honest savings, note dates, links --context
Author: abhi+claude | Date: 2026-10-18
Files: src/search.rs, src/render.rs, src/mcp.rs, src/main.rs, src/store.rs, src/spans.rs, src/graph.rs, src/code.rs, src/chunk.rs, src/db.rs, src/limits.rs

## Context
An outside review listed sixteen defects. The costly ones were a near-miss
gate that let a note about the same area but a different topic through, a
headline savings number that was an upper bound presented as a measurement,
the MCP search tool returning raw JSON several times the size of the CLI text,
chunks printed twice under `--sections`, hits dated by save time instead of
the note's own date, bare file names left unresolved in the graph, and no way
to see a definition's callers or constants from `links`.

## Decision
- Near-miss gate: a question's rare words (`search::distinctive_terms`, document
  frequency at most 10% of chunks, five letters or more, not in `GENERIC_WORDS`)
  are checked against the hit's heading and text together; the cosine is scaled
  by `NO_COVERAGE_FACTOR` plus the share present, graded rather than a cliff.
  Without the model, hits order by fused score alone; the quarter-bucket order
  by confidence applies only when the model ran, because word-overlap
  confidence is too coarse to bucket by and it defeated the section cue.
- `src/render.rs` renders the search answer once for the CLI and the MCP tool
  (`search` returns the same budgeted text plus `read[]`), so a tool call costs
  the same tokens by either path. `tidy_files` drops a bare name when the full
  path is listed and shows four files. Chunks that sit inside a printed
  Decision/Rejected/Measured section are not repeated.
- Savings: the `estimated` line is now `ceiling` and says it is an upper bound.
  `observed` is net of the search and `links` output in the same scope
  (`Observed.lookup_tokens`), its floor is a 40-line range read
  (`RANGE_READ_LINES`, what a grep hit plus context costs) instead of 150, and
  the line counts guided reads followed by another read of the same file
  (`Observed.rereads`): the span was not enough.
- Hits and `links` show the note's `Date:` header (`notes.dated`, schema v17,
  `chunk::dated`, `render::parse_date`) and fall back to the save time.
- A bare or partial file name in a note resolves at save to the one indexed
  path it names (`code::paths_ending_with`, `LINKS_VERSION` 4); an ambiguous
  name is kept as written and the saver is warned.
- `links <symbol> --context` (`spans::context`, `CONTEXT_ITEMS` 4) adds the
  constants the body names and the first call line in each using file.
- Snippet marker 4; plugin and crate 0.6.0.

## Decision (follow-up, same day)
- Reviewer retest on 0.6.0: one rare word present no longer gives 0.50 on its
  own (coverage of distinctive words scales by count over four, no floor), so
  "Blur background behind status photo" falls to 0.30 at the gate. Distinctive
  words may be four letters ("blur"), and a capitalised acronym of three
  ("NDK") counts, which brought the NDK question back. Kotlin `val` is its own
  kind (`val`), not `const` (code v7).
- The MCP ordering and 9 KB JSON the reviewer saw were the 0.4.0 server still
  running: 0.6.0 returns the CLI text (3.4 KB for that query, correct hit
  first). Restart Claude after `claude plugin update`.

## Rejected
- A hard near-miss cut (drop the hit when no rare word is present) lost two
  gold questions whose rare word was a generic verb ("removed", "reach"); the
  graded scale plus the generic-word list keeps recall@3 at 1.0 and still
  returns "no relevant note" for the backdrop question.
- Including four-letter words as distinctive: "many", "keep", "last" are rare
  in a small store without being topical.

## Measured
- Gold set, 44 questions: recall@3 1.000, recall@10 1.000, MRR 0.928 (before
  this batch 1.000 / 1.000 / 0.905; mid-batch low 0.932 / 0.869).
- "how is the blurred backdrop behind the status editor rendered" on
  AbhiGram: no relevant note (best 0.28 under 0.30); before, app-bugs at 0.67.
- Observed all time after the change: 2k-47k tokens (19-81%) net, was
  9k-54k (72-94%) gross with the 150-line floor; 4 of 6 guided reads were
  followed by another read of the same file.

## Open
- `links --context` finds callers; constants appear only when the body names
  an indexed `const` by its bare identifier (paths like `limits::X` are split
  by `identifiers`, so the name still matches; verify on a Kotlin file).
- BEAM baseline run still needs an API key.
