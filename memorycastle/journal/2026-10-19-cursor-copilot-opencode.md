# Install wires Cursor, GitHub Copilot and OpenCode too
Author: abhi+claude | Date: 2026-10-19
Files: src/install.rs, src/main.rs, README.md

## Context
Four agents were wired. The user asked which other popular agents to
support; the three with the largest reach that all speak MCP and read
`AGENTS.md` were added in one pass.

## Decision
`Agent` gains `Cursor`, `Copilot`, `OpenCode`. Each is detected by its
config dir or binary and gets the MCP entry in its own file:
`~/.cursor/mcp.json` (`mcpServers`, stdio), `~/.copilot/mcp-config.json`
(`mcpServers` with `type: local` and `tools: ["*"]`),
`~/.config/opencode/opencode.json` (`mcp` with `command` as an array and
`enabled: true`). `json_mcp` takes the key and the entry, so a new agent is
one line. OpenCode reads `~/.config/opencode/AGENTS.md`, so it gets the
user-level section; Cursor and Copilot keep user rules in the app and read
the repo's `AGENTS.md`, which `install` writes inside a repo. Version 0.10.2.

## Rejected
- Windsurf, Cline, Roo, Kilo, Aider: smaller or without MCP; add on request.
- Cursor `.cursor/rules/*.mdc`: Cursor reads `AGENTS.md` since 2025, and
  one file for five agents beats a second format.

## Measured
Fresh `HOME`, all seven named: twelve `[did]` lines, every file in the
expected place, the three new JSON files well formed. On this machine
`install` detects Cursor from `~/.cursor`; Copilot and OpenCode are absent
and skipped unless named.

## Open
No session has been driven through Cursor, Copilot or OpenCode yet; the
entries follow each tool's documented format and need one real run each.
