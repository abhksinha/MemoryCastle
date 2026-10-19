# Own gold set: 53 questions over this repo's journal
Author: abhi+claude | Date: 2026-10-18
Files: src/search.rs, src/spans.rs, src/eval.rs

## Context
The private AbhiGram gold set was the only measure of search quality. This
repo's 18 notes needed their own, written by a fresh reader (not the note
words), so that the near-miss gate added today could be judged on a second
store. Kept at `~/.local/share/memorycastle/gold-memorycastle.json`
(`eval --gold`), 47 questions with an answer and 6 absent topics.

## Decision
- The near-miss gate is weighted by how many rare words the question had
  (`min(n,3)/3`): one absent rare word costs at most 23% of the cosine, three
  cost 70%. One word was often the asker's synonym ("scored", "derived",
  "long") and a hard gate on it dropped the right note.
- Coverage matches a stem (`search::stem`: strip ing/ies/ed/es/ly/s, keep at
  least four letters), so "collapsed" finds "collapse" and "reindexing"
  finds "reindex".
- `read first:` prefers, among a note's linked symbols, the ones whose last
  name segment shares a stem with the question (+0.05, "name matches
  question"), so a question about the gate points at `distinctive_terms`
  rather than the note's first constant.

## Rejected
- Hard gate on any absent rare word: own gold recall@3 0.868 with seven
  misses, all right notes present in the store. Softened, 0.981.
- Keeping "blurred backdrop" (AbhiGram) at "no relevant note": with the
  softened gate it returns the status-editor-overlays Decision at 0.51, which
  does describe the frosted blur. Accepted; the five absent-topic AbhiGram
  questions and the six here still return nothing.

## Measured
- Own gold, auto: recall@3 0.981, recall@10 1.000, MRR 0.948; cold p50 53 ms,
  p95 62 ms, peak RSS 288 MB (second run p50 0.5 ms from the query cache,
  62 MB). The one miss is a gold ambiguity: the reindex time is in two notes
  and both rank above the one the gold names.
- Own gold, keyword only: recall@3 0.830, MRR 0.777, p50 0.4 ms, 7 MB. Misses
  are synonyms the words cannot bridge and one identifier the note never
  wrote (`GRAPH_DIRECT_WEIGHT`, gold error; the model still finds it).
- AbhiGram gold unchanged: recall@3 1.000, recall@10 1.000, MRR 0.905.
- Spans: 12 random definitions, printed start line holds the name 12 of 12.
- MCP `search` text 5.1 KB for a 3-hit sections answer, same text as the CLI.
- 47 tests pass; clippy clean.

## Open
- "apple neural engine" misses the CoreML note in every mode: vocabulary gap,
  no word shared. A synonym line in the note (`ANE`, "Neural Engine") is the
  honest fix, not a ranking change.
