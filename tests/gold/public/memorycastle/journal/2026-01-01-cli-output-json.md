# The CLI prints JSON when not on a terminal
Author: fixture+claude | Date: 2026-01-12
Files: `src/cli.rs`

## Decision
`tidepool get` prints plain text on a terminal and JSON when stdout is a pipe (`atty` check), so scripts never parse prose.

## Rejected
- A `--json` flag only: every script must remember it, and most forgot.
