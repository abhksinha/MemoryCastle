# Keys with a TTL expire lazily at read and compaction
Author: fixture+claude | Date: 2026-01-23
Files: `src/ttl.rs`

## Decision
An expired key is hidden at read time and removed at compaction (`Ttl::is_expired`). No timer thread.

## Rejected
- A background sweeper: scanned the whole keyspace every minute for a few expiring keys.

## Measured
- CPU at idle with 1 million TTL keys: 0.1% instead of 6%.
