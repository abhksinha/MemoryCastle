# The manifest is replaced atomically by rename
Author: fixture+claude | Date: 2026-01-06
Files: `src/manifest.rs`

## Decision
A new manifest is written to a temporary file, fsynced, then renamed over the old one; the directory is fsynced after.

## Rejected
- Appending edits to one manifest log: a torn append corrupted the whole file in a crash test.

## Measured
- Crash test: 0 corrupt manifests in 2,000 kills.
