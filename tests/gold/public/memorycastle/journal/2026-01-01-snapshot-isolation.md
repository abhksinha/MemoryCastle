# Reads use snapshot isolation through sequence numbers
Author: fixture+claude | Date: 2026-01-01
Files: `src/snapshot.rs`, `src/db.rs`

## Decision
A `Snapshot` pins a sequence number; reads ignore entries newer than it. Compaction keeps any version a live snapshot can see.

## Rejected
- Read locks on the memtable: writers stall behind long scans.

## Measured
- A 30 s scan no longer blocks writes; write p99 unchanged.
