# Ranked lines to read after every search, and savings per session
Author: abhi+claude | Date: 2026-10-18
Files: src/spans.rs, src/main.rs, src/mcp.rs, src/store.rs, src/db.rs, src/limits.rs, plugin/hooks/session-start.sh

## Context
The user asked for the exact lines to read, ranked, with both relative and absolute paths, and for savings shown for this session beside all time. Pinpointing lines is the point of the tool.

## Decision
`search` ends with `read first:` lines from `spans::ranked`: question symbol defined in the code index 1.0; symbols the top notes link, by note rank 0.9 decaying 15% per rank; the first line where a using file mentions the symbol, three lines either side, 0.6; definitions whose name contains a question content word 0.5. Ties to the shorter span, at most `READ_SPANS` 5, bytes per span shown. Definition spans re-anchored; use spans fresh. `--abs` or a cwd outside the root prints absolute paths; MCP JSON carries both. Code is not embedded: the graph ranks. Session id from the Claude Code session-start hook is stored per project and tags every read and call (schema v13); `status` and the status line show this session and all time.

## Rejected
Embedding code lines: hours of CPU for 20k definitions, and code is looked up, not remembered. Printing the enclosing definition for a use: `run` in `src/main.rs` is 478 lines; the use line with context is 7. Keeping a single all-time number: the user wants to see what this session did.

## Measured
Keyword search with ranked spans 18 ms end to end. Question "how does record_read decide a read is guided": five spans, 1.6 KB in all, `record_read` first; the use line in `src/main.rs:491-497`. Question with no symbol ("confidence floor and cosine rescale") got `COSINE_FLOOR`, `COSINE_CEIL`, `MIN_CONFIDENCE` by name match, 0.2 KB. Session line after one search and one guided read: 1k-9k tokens (87-97%).

## Open
Name match on a common word (`search`) pulls an unrelated constant at 0.5; it sits last. Enum variants and nested names are still not definitions, so they cannot be read spans. No integration test for `spans::ranked` yet; it needs a code index fixture.
