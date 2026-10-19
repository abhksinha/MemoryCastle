# status warns when an instruction file's memorycastle section is outdated
Author: abhi+claude | Date: 2026-10-18
Files: src/app.rs, src/store.rs, src/main.rs

## Context
The agent snippet changed three times today; repos that ran `init` earlier kept the old text silently. Done as a self-test of the tool: search, `links`, read only printed ranges.

## Decision
The snippet ends with an HTML comment marker `<!-- memorycastle snippet N -->` (`app::SNIPPET_MARKER`), bumped when the snippet changes. `status` warns when a file has the `## Memory (memorycastle)` heading but not the current marker, and says to replace the section with `memorycastle init` output. The old `journal/` warning now carries its line and reason in the same list.

## Rejected
Rewriting the section in place from `status`: the files belong to the user and may hold edits inside the section. Comparing the whole section text: any local tweak would warn forever.

## Measured
Bytes read to do the task with the tool: 4,241 (search plus two `links`) and 1,293 (two printed ranges and one grep), 5,534 in all, about 1.4k tokens. The files those ranges came from total 62,990 bytes (store.rs, main.rs, app.rs) plus a 21,459-byte journal, about 21k tokens if read whole. Search surfaced the span re-anchoring note and the open item that motivated this task; `links stale_instructions` gave `src/store.rs:468-486`, which was the whole function.

## Open
`init` prints the snippet; it does not splice it into a file. A `init --write` that replaces the section when it is the last one in the file would close the loop.
