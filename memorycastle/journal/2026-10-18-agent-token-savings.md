# Agent token savings measured with Codex, and search --sections
Author: abhi+claude | Date: 2026-10-18
Files: src/main.rs, src/chunk.rs, src/limits.rs, src/app.rs

## Context
The user asked whether memory and the code graph help an agent, and for the average saving in retrieval. Measured with `codex exec` in read-only mode, same question with the memory rules versus rg/cat only, two runs per mode.

## Decision
`search --sections` prints each hit's note Decision, Rejected and Measured sections once per note, bounded to `SECTION_LINES` (30) each, so an agent answers from search output instead of opening files. Agent snippets tell the agent to read these, not the files.

## Measured
AbhiGram (8 recovered notes, 79 KB, table-heavy), six "what did we decide" questions: with memory 15,423 tokens average, without 19,508; saving 21% (best 56%, one question lost 30% because its note is a 45 KB table and search hit it three times). File reads per question 7 to 38 even with memory.
This repo (9 structured notes), six questions with `--sections`: 7,737 vs 7,966; saving 3%, within noise. One run was inflated by a Codex plugin reading a skill file. Grep agent reads 2 to 4 small files and sits at the run floor, so there is nothing to save.
Code-plus-memory question ("constraints, definitions, Supersedes") on both repos: memory cost 8 to 25% more; the agent ran search and then read source anyway. Run-to-run variance is 2x on identical prompts.

## Rejected
Claiming a fixed saving. It scales with journal size: tiny journal, grep is as cheap. Storing git line ranges in SQLite: lines shift each commit; a note-to-commit hash link is the useful edge, not built yet.

## Open
Agents did not reach for `links <symbol>` over rg for definitions; the instruction may need to say so. Re-measure when the journal passes 50 notes.

## Measured (budgeted output, same day)
Search output was the waste: three calls of thin six-line snippets, 5k to 8k tokens, then the file anyway. Now one block per note (best chunk whole up to 1,200 bytes, two sibling chunks, sections), 6,000 bytes per call, long lines and file lists cut. Three AbhiGram questions, two runs each: retrieval tokens 3,092 / 2,097 / 2,712 against grep 5,567 / 4,634 / 30,323, saving 44%, 55%, 91% (average 63%); whole run 32%, 53%, 33%. The agent still opened the note once per question because these notes have no Decision section, so `save` now warns on notes over 1,500 words or without one.

## Measured (Antigravity, same day)
Same six questions through `agy --output-format json`, two runs per mode: with memory 42,087 tokens average, without 54,849, saving 23%, six of six cheaper (11% to 33%). Absolute saving per question 12.8k, the same as Codex; the percentage is smaller because Antigravity's one-shot floor is about 33k. One-shot runs never read the prompt cache (`cache_read_tokens` 0), so the floor is paid in full each run; a multi-turn session test follows.

## Measured (one session, same day)
Six questions as six turns of one Antigravity conversation, two repeats per mode. Caching holds in sessions: after turn 2, 17k to 45k read from cache per call, 2.5k to 4k new. Averages with memory vs grep: 22 vs 32 model calls, new input 102k vs 149k, cache reads 652k vs 1,108k, output 11k vs 15k, context at the end 114k vs 165k. Cost at Gemini 3.8 Flash paid rates: $0.167 vs $0.253, 34% saved; 45% if nothing were cached. The percentage barely moves with cache weighting because both modes cache equally; what differs is the new text each mode drags in and re-reads. Grep repeats differed by 25%, memory repeats by 4%.
