# Hooks for Codex, Gemini CLI and Cursor from one binary entry point
Author: abhi+claude | Date: 2026-10-19
Files: src/hook.rs, src/install.rs, src/main.rs, src/draft.rs, src/limits.rs, plugin/hooks/recall.sh

## Context
The reminders that make a note happen (recall on each prompt, the
mid-session owed line, the hold before compaction and at stop, the line
after a commit) ran only in Claude Code, through the plugin's shell scripts.
Asked whether other agents get them too: they did not, and their edits never
reached the ledger, so `owed` had nothing to count there.

## Decision
- `memcastle hook <agent> <event>`: one Rust entry point reads the agent's
  JSON, runs the same subcommands the scripts run by spawning its own binary,
  and answers in the agent's output shape. Codex, Gemini CLI (Antigravity
  reads the same settings file) and Cursor; seven events mapped to each
  agent's names and matchers in `hook::entries`.
- `install` writes the hook entries beside the MCP entry; ours are recognised
  by the command text, so a rerun replaces and `uninstall` removes only ours.
  Codex needs `/hooks` once to trust them.
- Mid-session reminder (`owed --remind`, also added to the Claude prompt
  hook): fires when 5 files were edited since the last note or the oldest
  such edit is 30 minutes old, then not for 30 minutes. Rare and true.

## Rejected
- Shell scripts per agent: three copies of jq and python parsing, and Gemini
  and Cursor differ in output keys; the binary already has the JSON parser.
- A reminder on every prompt while a note is owed: the agent learns to skip
  it; the threshold and cooldown keep it to one line per half hour at most.
- Cursor `beforeSubmitPrompt` for recall: it can only block or let through,
  no context field, so recall is not wired there; sessionStart, afterFileEdit,
  afterShellExecution and stop are.
- Hooks for Copilot, OpenCode, Antigravity: no hook surface documented.

## Measured
- Codex prompt: plain text with the recall lines; Gemini: the same inside
  `hookSpecificOutput.additionalContext`, `{}` when nothing; Cursor
  sessionStart: `additional_context` with the status lines.
- Codex `apply_patch` before-edit placed the edit at `is_identifier_use`
  from the patch's first removed line and printed the one note about it.
- Five recorded edits: the first prompt printed the owed line, the second
  nothing. 64 tests pass.

## Open
- Not yet exercised inside a real Codex, Gemini or Cursor session; the
  field names come from their current docs. First real session in each
  should check `status` shows the session and `owed` counts its edits.
