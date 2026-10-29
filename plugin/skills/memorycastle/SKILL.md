---
name: memorycastle
description: Search the repo's memory of decisions, rejected attempts and measurements before designing, fixing or resuming; save a journal note when a task changes a decision. Use when the user refers to the past, when choosing an approach in an area with history, or when a search hook prints notes about a file.
---

# memorycastle

Run `memcastle init` to see the full rules; the short form:

- Before a design choice, a fix that may have been tried, or resuming work:
  `memcastle search "<question>" --also "<same question in the repo's own words>" --sections`. Read the printed Decision,
  Rejected and Measured bullets and the `code:` ranges; do not open the files.
  Bullets that did not match print as `… lines a-b`; `--whole` prints every line.
- A plain `search` (and the MCP tool by default) prints one line per item, verb first: `read note.md:a-b  rejected: <sentence>` per note, best first (a parenthesis at the end says superseded or may be stale), and `open path:a-b  name, called by ...` for code the question names. Do what the line says and read only that range with `sed -n 'a,bp' path`; `--sections` on the note you pick.
- Before editing a file or symbol: `memcastle links <file|symbol>`; read
  only the printed `path:start-end` with `sed -n 'a,bp'`. For a symbol it
  prints where it is defined, who calls it and what it calls; for a file
  with no notes, what it defines and who uses it; `--at <line>` gives the
  function holding the line. It works before the first note is written.
- `no note on this; read the code` means the store has nothing; do not act on a weak hit.
- Searches learn by themselves: when you read a note the search did not
  rank first, the question is kept for that note and the same question in
  other words finds it next time. Read the note that answered, not a guess.
  `memcastle learned` lists them; `learned forget <id>` drops a wrong one.
- At the end of a task that changed a decision, measured something, or
  rejected an approach: write `memorycastle/journal/YYYY-MM-DD-<topic>.md`
  with Author, Files, Decision, Rejected, Measured, Open; then
  `memcastle save --changed`. When the work continues an earlier note,
  edit that note instead and save it again; to reverse its decision, write
  a new note with `Supersedes:`. Never delete a note.
- `memcastle status` shows savings as a percentage of bytes not read.
