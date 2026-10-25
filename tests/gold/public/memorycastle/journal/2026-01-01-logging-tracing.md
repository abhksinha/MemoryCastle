# Logging goes through the tracing crate
Author: fixture+claude | Date: 2026-01-09
Files: `src/log.rs`

## Decision
All logs are `tracing` events with structured fields; the default subscriber writes one line per event.

## Rejected
- println debugging left in releases. The log crate: no spans for timing compactions.
