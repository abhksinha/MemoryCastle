# Recall on every prompt, notes that know their code changed, edit hook by symbol
Author: abhi+claude | Date: 2026-10-19
Files: src/main.rs, src/search.rs, src/graph.rs, src/code.rs, src/app.rs, src/render.rs, src/limits.rs, src/db.rs, plugin/hooks/recall.sh, plugin/hooks/before-edit.sh

## Context
Asked what would make memory and the graph more correlated, and then which
work is most effective per hour, three things were chosen over a reranker,
a call graph and code embeddings: each is deterministic, each closes a gap
seen in real use this week, and each needs no new model. The label log
(five served, one opened) showed the agent searches rarely even when the
notes are its own; the `links` hook showed every note about a file when the
edit touched one function; and no hit could say whether the code it
describes still looks the way it did.

## Decision
- `memcastle recall` and a `UserPromptSubmit` hook: the prompt is searched
  locally and a hit at or above `RECALL_MIN_CONFIDENCE` 0.50 prints one line,
  once per note per session, at most three; a prompt with fewer than three
  content words asks nothing. 0.50 is cosine 0.70: above the off-topic peak
  of 0.65 and under the right note's median of 0.75, so a task stated in the
  agent's words ("make embedding faster by batching chunks") still recalls
  the note that rejected it (0.53).
- `note_code` (schema v20): at save, the body hash of every definition the
  note names. `search` compares, labels `code changed since: <symbols>` and
  multiplies the score by `CODE_CHANGED_WEIGHT` 0.7. Symbols match by bare
  name (`graph::same_symbol`), so a note writing `spans::context` is about
  `context`; this also fixed `links` by bare name and the graph rerank.
- `links <file> --at <line>`: the innermost definition holding the line
  (`code::def_enclosing`) becomes the target when a note names it, printed
  as `about: <symbol>`; `before-edit.sh` finds the line from `old_string`.

## Rejected
- A recall gate at 0.70 confidence: half the right notes sit under it
  (cosine median 0.75 maps to 0.75, but a task phrased as an instruction
  scores lower than a question); 0.70 missed the batching note.
- Injecting recall on every prompt without the once-per-session rule: the
  same note would cost 15 tokens on every follow-up in a long session.
- A learned reranker, a call graph, code embeddings: no measured miss yet;
  each needs the label log to show the hand rules fail first.

## Measured
- Code changed: editing one comment line inside `is_identifier_use` moved
  the note that names it from first (0.77) to second with the label; the
  note's text was untouched. 63 tests pass.
- Recall: "make embedding faster by batching chunks" prints the one-chunk
  note at 0.53 once; "yes" and "run the tests again" print nothing; the
  second identical prompt prints nothing.
- Edit hook: an edit inside `is_identifier_use` in `src/spans.rs` prints the
  one note that names it instead of the three notes about the file.

## Open
- Whether recall lines get opened: the label log records them as mode
  `recall`; read the opened share after a few weeks of real sessions.
- `code changed` is per symbol the note names; a note that names only files
  cannot be checked and is not labelled.
