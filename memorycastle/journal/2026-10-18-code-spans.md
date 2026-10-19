# Code spans in results, re-anchored when the file changes
Author: abhi+claude | Date: 2026-10-18
Files: src/code.rs, src/main.rs, src/graph.rs, src/db.rs, src/limits.rs

## Context
A search hit told the agent why; the agent then opened the whole file to find where. Hypothesis: a result that gives the decision first and then an accurate `path:start-end` turns the "open the file" step into a 50-line read.

## Decision
Every definition stores its end line and its anchor line (schema v9). End by brace depth with strings and comments blanked, by indentation for Python, same line for Kotlin expression bodies, bounded at 2,000 lines. At output, `current_span` compares the file hash with the index; unchanged prints the stored span, changed re-finds the anchor line (then the name) and recomputes, missing says moved. `links <symbol>` and `search --sections` (up to four symbols per note) print the span; graph nodes carry it in `loc`.

## Rejected
Storing spans without re-anchoring: a stale span sends the agent to the wrong lines, worse than none. A parser per language for exact boundaries: the agent has an LSP for that; the memory tool needs a jump target, not a syntax tree.

## Measured
40 random function definitions, Rust and Kotlin: 38 spans correct; both misses are blocks whose closing brace sits at a different indent. Inserting three lines at the top of StatusRender.kt: `64-118` printed as `67-121` with no reindex. Index of 974 files: 0.80 s cold with spans and anchors.

## Open
Whether agents use the span instead of `cat` is the token test to run next: the embed_missing and CropRect questions, both modes. Enum variants and nested names remain unindexed.

## Measured (agent test, same day)
Two code-plus-memory questions (CropRect, emojiBitmap) through Codex, memory vs grep, two runs each, eight runs in parallel. In all four memory runs the agent did `links <symbol>`, one `search --sections`, then `sed -n` on the printed range, never `cat`. Retrieval tokens: CropRect 2.3k vs 13.1k (83% less), emojiBitmap 2.8k vs 7.9k (65% less). Whole run: CropRect 15.5k vs 32.7k (53% less); emojiBitmap 23.0k vs 15.0k, worse, because a Codex plugin read a 4 KB skill file in both memory runs and one grep run found the symbol with a single rg and sed. This question class cost 8 to 25% more with memory this morning; the span turns it, on retrieval in both cases and on the whole run in one.
