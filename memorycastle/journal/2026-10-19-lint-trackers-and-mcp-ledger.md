# lint tells a tracker to archive closed entries; MCP links joins the ledger
Author: abhi+claude | Date: 2026-10-19
Files: src/store.rs, src/mcp.rs

## Context
First real task through the 0.7.0 hooks, to see what the ledger says. The
task: the verbosity check found AbhiGram trackers of 2,700 and 5,900 words,
and `lint` only said "split it".

## Decision
- `lint` (`store::lint`): an undated note over `NOTE_MAX_WORDS` with closed
  `- [x]` entries is told to move those to `<name>-archive.md` so open items
  rank first; a dated note still gets "split into one decision each".
- MCP `links` now does what the CLI does: re-anchors spans with
  `current_span`, records them as served and the call in `usage`, returns
  text with absolute paths. Before, a ranged read after an MCP `links` call
  was never counted as guided, and the call cost nothing in the ledger.
- MCP `search` defaults to the 3-line snippet; `sections: true` on request.
  With sections on, a two-note answer printed 6.0 KB for notes totalling
  6.6 KB: the lookup cost nearly the whole notes.

## Rejected
- Keeping sections on by default for MCP because it "makes one call enough":
  the ledger showed it making one call cost as much as reading the notes.

## Measured
- This task, through the hooks: the read guard stopped one whole-file Read
  and the ranged retry was credited (1.4 KB of 32.6 KB); the `lint` span
  read via `sed` was not credited because MCP `links` served no span (fixed).
- MCP `search` for the same two-note question: 1.7 KB snippet mode, 6.0 KB
  sections mode. MCP `links lint`: 68 bytes, span 856-902 re-anchored after
  the edit.
- 48 tests pass; clippy clean.

## Open
- Session floor stays 0 while the session carries the test runs' lookup
  cost; a fresh session measures the hooks alone.
