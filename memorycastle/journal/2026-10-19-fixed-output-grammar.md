# Search output follows a fixed grammar; dated notes get their date on insert
Author: abhi+claude | Date: 2026-10-19
Files: src/render.rs, src/store.rs, src/importlog.rs, docs/GUIDE.md, tests/store.rs

## Context
The user found the search answer "a little odd" and asked for XML, then
TSV, then Markdown. Each serves one consumer and hurts another: XML costs
40 to 60% more tokens per lookup and reads badly in a terminal; TSV cannot
hold prose with line breaks; Markdown is syntax in a plain terminal and has
no grammar for fields. The machine side was already structured (`--json`,
MCP `read[]`); the eyes side was the problem.

## Decision
One text form with a fixed grammar, JSON beside it. Hit line
`n. path:line   confidence   section`; title and author; `label value`
fields with the label padded to `LABEL_WIDTH` (10) and continuations at
`VALUE_COL` (14), prose wrapped at `WRAP_COLS` (92); a blank line ends a
hit; `read first` rows lettered `a.` so they never read as hits, or
`read first: none`. A note's own line that looks like a hit or a label can
never reach column 0 or the label column. `render_follows_the_fixed_grammar`
is the gate. `memcastle guide` prints `docs/GUIDE.md`, the usage written as
instructions an LLM can follow literally; `--help` points at it.
Version 0.10.5.

## Rejected
- XML tags: tokens and terminal noise; agents read text, parsers use JSON.
- TSV: no prose; columns collapse on one long path.
- Markdown as default: syntax for humans in a terminal, no field grammar;
  a `--md` flag over the JSON if a chat UI ever wants it.

## Measured
Same bytes as before within a few percent; the sections view of a real
note rendered with every line classifiable as hit, title, label or
continuation. Found by the test's output: a note added fresh never got its
`dated` column, only an updated one did (the insert omitted it), so dated
notes printed a date in the hit line. Fixed; 22 notes backfilled across
three stores; 8 trackers stay undated by design.
