# Shutdown drains writes, then flushes the memtable
Author: fixture+claude | Date: 2026-01-27
Files: `src/db.rs`

## Decision
`Db::close` stops accepting writes, waits for in-flight group commits, flushes the memtable and fsyncs the manifest.

## Rejected
- Exiting without flushing: correct (the WAL replays) but every restart took 20 s of replay.

## Measured
- Restart after clean shutdown: 20 s to 0.3 s.
