# One install command wires every agent; the ledger imports Codex rollouts
Author: abhi+claude | Date: 2026-10-19
Files: src/install.rs, src/importlog.rs, src/serve.rs, src/bin/memcastle.rs, plugin/hooks/session-start.sh

## Context
Setup was four hand steps per agent and the savings ledger saw only Claude
Code hooks. The user asked for `memcastle install --all` that finds the
agents and adds the memory section to the files they read, and for the
embedding server to be warm from session start.

## Decision
`memcastle install [claude,codex,gemini,antigravity]` detects agents by
config dir or binary and, per agent, writes the MCP entry, the Claude
plugin and status line, and the memory section in `CLAUDE.md`, `AGENTS.md`
or `GEMINI.md` (an older section is replaced up to its marker). Per repo
it builds the code index, saves notes, imports every Claude Code and Codex
transcript recorded in that root, and starts `serve` detached. The
session-start hook runs `serve --detach` so the first lookup is warm.
`memcastle` is a second binary built from an include of `main.rs`.

## Rejected
- Editing `~/.claude/settings.json` hooks by hand instead of the plugin:
  the plugin is versioned and updated by Claude; duplicating its hooks
  would drift.
- Gemini CLI and Antigravity hooks: neither has a hook API on this machine
  to verify against; they get the MCP server, the skill text and, for
  Codex, the transcript importer. Antigravity's conversation format is not
  imported.
- Replacing a user's own status line: left alone unless it is ours.

## Measured
- `memcastle install` on this repo: 1.2 s; 37 files and 609 definitions
  indexed, 17 transcripts imported (297 reads, 7 lookups). AbhiGram: 805
  files, 19,907 definitions, 37 transcripts (32 reads, 30 Codex lookups).
- Warm server: a meaning search reports `model warm`; gold runs p50 1.1 ms
  on cached queries.
- Gold after today: AbhiGram recall@3 1.000, MRR 0.894; own set 0.962,
  0.872 (two ambiguous questions rank third behind the day's new notes).

## Open
- Codex rollouts from the benchmark era show 30 lookups and no guided reads:
  Codex reads through its own tools, not `cat`, so its reads are counted but
  never matched to a span. A Codex-side read ledger needs its tool-call
  records parsed for file paths.
