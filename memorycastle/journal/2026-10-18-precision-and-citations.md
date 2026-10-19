# Precision, citations and the reviewer's list
Author: abhi+claude | Date: 2026-10-18
Files: src/search.rs, src/chunk.rs, src/graph.rs, src/project.rs, src/store.rs, src/limits.rs, src/eval.rs

## Context
An expert reviewer used memorycastle on AbhiGram for a day and listed ten issues: no "nothing found", file links split by path style, the wrong section ranking first, tracker entries as wall-of-text chunks under a heading called "Low", duplicate facts filling slots, the journal move not announced, `--related` not reaching the sister repo, manual per-file save, no line numbers to cite, and a second memory system adding noise. The philosophy adopted: search returns the sentence that answers, with a score and a line, or says nothing.

## Decision
Confidence per hit: rescaled cosine (floor 0.60, ceiling 0.80) when the model ran, content-word coverage otherwise, 1.0 for a verbatim identifier; under 0.30 the hit is dropped and search prints "no relevant note". Section cue from the question's wording weights Measured, Decision or Rejected 1.3x. Duplicates collapse by normalised head with the tracker id stripped, dated note preferred. Chunks store their line (schema v8); hits print `note.md:line` and a sentence-centred snippet; a chunk starting with `B-42` is labelled by it; table pieces repeat the header row; `CHUNK_VERSION` rechunks once. `links` matches bare, partial and full paths as one file and prints the spelling each note used. `--related` without a group keeps isolation and prints how to declare the group. `save --changed` saves every journal note; `status` warns when CLAUDE.md, AGENTS.md or GEMINI.md still say `journal/`. The gold set accepts an empty answer meaning "must return nothing".

## Rejected
`--related` falling back to every project in the store: the isolation test forbids it, and a sister must be declared, not guessed. Coverage counting stopwords: "what is X for" scored 0.75 against anything. One content word as proof of relevance when the model ran: "memorycastle" in passing kept an off-topic question alive; coverage is scaled by the number of content words, except in keyword mode where the words are all there is. Cosine floor 0.35: nothing was ever dropped.

## Measured
Cosine of the right note's best chunk on 24 gold questions: min 0.614 (an identifier, kept by coverage), median 0.745, max 0.834. Off-topic questions: 0.538 to 0.645. Gold set now 42 questions with five absent: auto recall@3 1.000, recall@10 1.000, MRR 0.921; keyword 0.952 / 0.952 / 0.857. `links StatusLayers.kt`, `links ui/status/StatusLayers.kt` and the full path return the same two notes. "did frosted shapes fit the memory budget" now ranks the Measured section first.

## Open
The second memory system (Cognee) is outside this tool; disable it in the agent config if its memories are noise. ML-KEM-1024 question misses in keyword mode because the note spells it differently. Thresholds are calibrated on one corpus; re-run `eval` after adding notes of a new kind.
