# Uninstall reverses install and leaves the notes
Author: abhi+claude | Date: 2026-10-19
Files: src/install.rs, src/main.rs, README.md

## Context
Install touched nine files across four tools and the README had no way
back. A tool that edits agent configs owes the user the reverse.

## Decision
`memcastle uninstall [agents] [--repo-only] [--purge] [--dry-run]`:
`remove_snippet` takes the memory section out of each user-level and repo
instruction file (the file is deleted when it held nothing else), the MCP
entry out of every agent config (`json_mcp_remove`, the Codex TOML block),
the Claude plugin and marketplace through the `claude` CLI, and our status
line script with its setting only when the script is ours. `--purge` deletes
the data dir (store, models, runtime) and the socket. Journal notes stay:
they are the user's files. The binary is not deleted by the program that is
running; the command to do it is printed. Version 0.10.3.

## Rejected
- Deleting `memorycastle/journal/` on `--purge`: the notes are the memory
  and committed with the code; a tool should never delete the user's prose.
- Removing a status line the user wrote: left alone unless it is ours.

## Measured
Scratch home with a pre-existing Codex config and `AGENTS.md`: install then
`uninstall --purge` left both files byte-identical to before, deleted the
OpenCode `AGENTS.md` it had created, and removed the data dir.
