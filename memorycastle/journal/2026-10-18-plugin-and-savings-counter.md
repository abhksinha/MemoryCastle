# Claude Code plugin with edit hook, and a savings counter in bytes
Author: abhi+claude | Date: 2026-10-18
Files: plugin/hooks/before-edit.sh, plugin/hooks/hooks.json, src/store.rs, src/main.rs, src/db.rs

## Context
The user wanted the tool installed as a plugin so agents use it without being told, and savings reported continuously in a way no tokenizer can distort.

## Decision
`plugin/` holds a Claude Code plugin: MCP server, a skill with the rules, hooks. `PreToolUse` on Edit, Write and MultiEdit runs `memorycastle links <file>` and prints the notes and symbol spans; `SessionStart` prints status; `Stop` reminds about unsaved notes. A `usage` table (schema v10) records per search and links call the bytes printed and the bytes of the whole notes and files the output drew from; `status` reports the saving as a percentage of bytes, cumulative and last 24 hours. Percent of bytes, because the ratio is tokenizer-independent.

## Rejected
Counting tokens with an estimate of 4 bytes per token: a number that depends on the agent's tokenizer and would be argued about. Having the hook rewrite or block the edit: it informs; the agent decides. Hooking every Read: too chatty, and reads are what the tool is meant to replace.

## Measured
First four calls in this repo after install: 8.1 KB printed for 46.7 KB of notes and files, 83% of bytes not read. The edit hook on `src/store.rs` printed three notes and ran in under 10 ms.

## Open
Only Claude Code has the hook; Codex and Antigravity get the MCP server and the instruction file. The counter assumes the whole file would have been read; a stricter view would count only files the agent then opened by range.
