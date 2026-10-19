# Search output as keyed lines under a titled block
Author: abhi+claude | Date: 2026-10-19
Files: src/render.rs, README.md, plugin/skills/memorycastle/SKILL.md

## Context
A hit line packed seven fields (path, line, project, heading, date, author,
confidence) into one row and the `read first` lines five; the user asked
for the same information in a structured form.

## Decision
`render::search` prints one block per note: `n. <title>  <confidence>`,
then keyed lines `note:` (path:line, section, date, author), `files:`,
`text:` or the lowercase `decision:`, `rejected:`, `measured:` sections,
and `code:` spans aligned by symbol. The `read first` list is a numbered
table with the location and name columns padded to their widest row. The
project tag appears only when hits span more than one project. The title
leads because it is the line the embedder saw with every chunk and states
the decision.

## Rejected
- JSON for the CLI: the MCP tool already returns `read[]` structured, and
  an agent reads the text; keyed lines cost the same bytes and stay greppable.
- Tables for section bodies: prose sections stay prose, as decided in
  `2026-10-19-note-shape-and-authorship.md`.

## Measured
Bytes for the same answer, old against new: 3,139 to 3,194 with
`--sections`, 887 to 852, snippet 1,275 to 1,339 and 406 to 407. Within 5%
either way; the `| ` prefix left and the key lines arrived.
