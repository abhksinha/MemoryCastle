---
name: memorycastle
description: Search the repo's memory of decisions, rejected attempts and measurements before designing, fixing or resuming; save a journal note when a task changes a decision. Use when the user refers to the past, when choosing an approach in an area with history, or when a search hook prints notes about a file.
---

# memorycastle

Run `memcastle init` to see the full rules; the short form:

- Before a design choice, a fix that may have been tried, or resuming work:
  `memcastle search "<question>" --sections`. Read the printed Decision,
  Rejected and Measured sections and the `code:` ranges; do not open the files.
- `search --brief` (or the MCP `brief` flag) prints one line per hit, about 15 tokens, to choose a note; then `--sections` on that one.
- Every search ends with a ranked `read first` table (`n. path:start-end  name  score  why`). Read those
  ranges with `sed -n 'a,bp' path` first; they are the lines the question
  needs, cheapest first on ties.
- Before editing a file or symbol: `memcastle links <file|symbol>`; read
  only the printed `path:start-end` with `sed -n 'a,bp'`.
- `no relevant note` means the store has nothing; do not act on a weak hit.
- At the end of a task that changed a decision, measured something, or
  rejected an approach: write `memorycastle/journal/YYYY-MM-DD-<topic>.md`
  with Author, Files, Decision, Rejected, Measured, Open; then
  `memcastle save --changed`. Never edit or delete an old note; supersede it.
- `memcastle status` shows savings as a percentage of bytes not read.
