# A panic in a background thread stops the store
Author: fixture+claude | Date: 2026-01-24
Files: `src/db.rs`

## Decision
Background threads catch a panic, mark the store read-only and log it; writes then return `Error::Busy` until restart.

## Rejected
- Restarting the thread: a corrupt table caused a panic loop that filled the disk with logs.
