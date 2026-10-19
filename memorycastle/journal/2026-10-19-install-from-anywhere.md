# Install wires the user-level instruction files, so memory works in every repo without a per-repo step
Author: abhi+claude | Date: 2026-10-19
Files: src/install.rs, src/main.rs, scripts/get.sh, plugin/hooks/session-start.sh, README.md

## Context
After the one-line installer the next step read `cd your-repo && memcastle
install`. The user asked why a repo was needed at all: the tool should work
for every agent in whatever repo they open.

## Decision
`memcastle install` from any directory now upserts the memory section into
the user-level instruction file each agent reads in every repo:
`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, `~/.gemini/GEMINI.md`
(`Agent::user_instruction_file`). Antigravity has no documented user-level
file and keeps the repo one. The MCP entries, plugin and status line were
already machine-level. Inside a repo, `install` still adds the section to
the repo's own file (for teammates who have not installed), builds the code
index, saves notes, imports transcripts and starts the server; the
session-start hook prints a one-line hint when a repo with a journal has no
section in its `CLAUDE.md`, and never edits the file itself. Version 0.10.1.

## Rejected
- The hook adding the repo section on first session: edits a file in a repo
  the user may not own, on a path they did not ask for. A hint instead.
- The installer running `install` itself: a curl-pipe script that edits
  agent configs unasked is the kind people rightly distrust. Two lines.

## Measured
Run from `~/Downloads`: user-level sections added or refreshed for Claude,
Codex and Gemini, every other step already current. The stale v0.10.0
release copy in `~/.local/bin` shadowed the cargo build on PATH during the
test; removed by hand.
